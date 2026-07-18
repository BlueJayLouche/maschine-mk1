//! Maschine Mk1 USB host session — Rust port of the proven C spike
//! (../firmware/src/main.c) over raw esp-idf-sys bindings, with all packet
//! codecs from mk1-protocol.
//!
//! Full-speed recipe (hardware-verified, see docs/protocol.md): the Mk1
//! serves a truncated illegal config (alt 0 only, EP 0x01/0x81, MPS 512).
//! Clamp the cached descriptor's MPS to 64, claim alt 0, rewrite the cache
//! in place so it describes the undeclared pad/display EPs as "alt 1",
//! claim that too, then SET_INTERFACE(1) — the device honours it.
//!
//! Threading: callbacks and the attach flow all run in the client thread
//! (inside `usb_host_client_handle_events`), so the statics below are
//! single-context; the display thread only ever touches its own EP 0x08
//! transfer and talks to the client context via atomics + channel.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::OnceLock;

use esp_idf_svc::sys::*;
use mk1_protocol as mk1;

const EP1_BUFSIZE: usize = mk1::EP1_BUFSIZE; // 64

static NEW_ADDR: AtomicU8 = AtomicU8::new(0);
static SESSION_UP: AtomicBool = AtomicBool::new(false);
static EP8_DONE: OnceLock<mpsc::SyncSender<bool>> = OnceLock::new();

// ponytail: raw statics, C-spike shape — everything below is touched only
// from the client thread (callbacks included). Wrap in a struct if a second
// context ever needs them.
static mut CLIENT: usb_host_client_handle_t = std::ptr::null_mut();
static mut DEV: usb_device_handle_t = std::ptr::null_mut();
static mut EP1_IN: *mut usb_transfer_t = std::ptr::null_mut();
static mut EP1_OUT: *mut usb_transfer_t = std::ptr::null_mut();
static mut EP4_IN: *mut usb_transfer_t = std::ptr::null_mut();
static mut EP8_OUT: *mut usb_transfer_t = std::ptr::null_mut();
static mut CTRL: *mut usb_transfer_t = std::ptr::null_mut();

// EP1 OUT command queue (callbacks may overlap a send in flight).
struct TxQueue {
    buf: [[u8; 40]; 8],
    len: [u8; 8],
    head: u8,
    tail: u8,
    busy: bool,
}
static mut TXQ: TxQueue = TxQueue {
    buf: [[0; 40]; 8],
    len: [0; 8],
    head: 0,
    tail: 0,
    busy: false,
};

struct Session {
    leds: mk1::leds::LedState,
    knobs: [Option<u16>; 11],
    buttons: mk1::input::ButtonStates,
    pads: [u16; 16],
}
static mut SESSION: Session = Session {
    leds: mk1::leds::LedState::new(),
    knobs: [None; 11],
    buttons: mk1::input::ButtonStates(0),
    pads: [0; 16],
};

unsafe fn ep1_pump() {
    let q = &mut *core::ptr::addr_of_mut!(TXQ);
    if q.busy || q.head == q.tail {
        return;
    }
    let i = (q.tail % 8) as usize;
    let n = q.len[i] as usize;
    let t = &mut *EP1_OUT;
    core::ptr::copy_nonoverlapping(q.buf[i].as_ptr(), t.data_buffer, n);
    t.num_bytes = n as i32;
    q.tail = q.tail.wrapping_add(1);
    q.busy = true;
    if usb_host_transfer_submit(EP1_OUT) != ESP_OK {
        log::error!("EP1 OUT submit failed");
        q.busy = false;
    }
}

unsafe fn ep1_send(data: &[u8]) {
    let q = &mut *core::ptr::addr_of_mut!(TXQ);
    if q.head.wrapping_sub(q.tail) >= 8 {
        log::warn!("EP1 queue full, dropping cmd {:#04x}", data[0]);
        return;
    }
    let i = (q.head % 8) as usize;
    q.buf[i][..data.len()].copy_from_slice(data);
    q.len[i] = data.len() as u8;
    q.head = q.head.wrapping_add(1);
    ep1_pump();
}

unsafe fn led_flush() {
    // LEDs coalesce in LedState's dirty banks — when the queue is backed up
    // (fast pad rolls at FS), skip and let the next event flush the latest.
    let q = &*core::ptr::addr_of!(TXQ);
    if q.head.wrapping_sub(q.tail) >= 6 {
        return;
    }
    let s = &mut *core::ptr::addr_of_mut!(SESSION);
    for frame in s.leds.frames() {
        ep1_send(&frame);
    }
}

unsafe extern "C" fn ep1_out_cb(_t: *mut usb_transfer_t) {
    let q = &mut *core::ptr::addr_of_mut!(TXQ);
    q.busy = false;
    ep1_pump();
}

unsafe extern "C" fn ep1_in_cb(t: *mut usb_transfer_t) {
    let tr = &*t;
    if tr.status == usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
        let buf = core::slice::from_raw_parts(tr.data_buffer, tr.actual_num_bytes as usize);
        handle_ep1(buf);
    } else {
        log::warn!("EP1 IN status {}", tr.status);
    }
    if !DEV.is_null() {
        usb_host_transfer_submit(t);
    }
}

unsafe fn handle_ep1(buf: &[u8]) {
    let s = &mut *core::ptr::addr_of_mut!(SESSION);
    match mk1::input::Ep1Message::parse(buf) {
        Some(mk1::input::Ep1Message::DeviceInfo(info)) => {
            log::info!("Maschine Mk1 says hello: firmware {}", info.fw_version);
            ep1_send(&mk1::auto_msg(1, 10, 5));
            s.leds.set(mk1::leds::Led::Backlight, 0x2e);
            led_flush();
            usb_host_transfer_submit(EP4_IN);
            SESSION_UP.store(true, Ordering::SeqCst);
        }
        Some(mk1::input::Ep1Message::Knobs(vals)) => {
            for (i, knob) in mk1::input::Knob::ALL.iter().enumerate() {
                let v = vals[i];
                if let Some(prev) = s.knobs[i] {
                    if (v as i32 - prev as i32).unsigned_abs() > 4 {
                        log::info!("{} = {}", knob.name(), v);
                        s.knobs[i] = Some(v);
                    }
                } else {
                    s.knobs[i] = Some(v);
                }
            }
        }
        Some(mk1::input::Ep1Message::Buttons(now)) => {
            for (b, down) in now.diff(s.buttons) {
                log::info!("button {} {}", b.name(), if down { "down" } else { "up" });
            }
            s.buttons = now;
        }
        Some(mk1::input::Ep1Message::Midi { data, .. }) => {
            log::info!("DIN MIDI in, {} bytes", data.len());
        }
        _ => {}
    }
}

unsafe extern "C" fn ep4_in_cb(t: *mut usb_transfer_t) {
    let tr = &*t;
    if tr.status == usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED {
        let s = &mut *core::ptr::addr_of_mut!(SESSION);
        let buf = core::slice::from_raw_parts(tr.data_buffer, tr.actual_num_bytes as usize);
        for (raw, mut pressure) in mk1::input::parse_pads(buf) {
            if pressure < 6 {
                pressure = 0;
            }
            let printed = mk1::input::pad_number(raw);
            let prev = s.pads[raw as usize];
            if pressure.abs_diff(prev) >= 128 || (pressure == 0 && prev != 0) {
                log::info!("pad {} pressure {}", printed, pressure);
                s.pads[raw as usize] = pressure;
            }
            s.leds
                .set(mk1::leds::Led::pad(printed as usize - 1), (pressure >> 6) as u8);
        }
        led_flush();
    }
    if !DEV.is_null() {
        usb_host_transfer_submit(t);
    }
}

unsafe extern "C" fn ep8_out_cb(t: *mut usb_transfer_t) {
    let ok = (*t).status == usb_transfer_status_t_USB_TRANSFER_STATUS_COMPLETED;
    if let Some(tx) = EP8_DONE.get() {
        let _ = tx.try_send(ok);
    }
}

unsafe extern "C" fn ctrl_cb(_t: *mut usb_transfer_t) {
    log::info!("SET_INTERFACE alt 1 done, starting session");
    usb_host_transfer_submit(EP1_IN);
    ep1_send(&mk1::get_device_info());
}

unsafe extern "C" fn client_cb(msg: *const usb_host_client_event_msg_t, _arg: *mut core::ffi::c_void) {
    let m = &*msg;
    if m.event == usb_host_client_event_t_USB_HOST_CLIENT_EVENT_NEW_DEV {
        NEW_ADDR.store(m.__bindgen_anon_1.new_dev.address, Ordering::SeqCst);
    } else if m.event == usb_host_client_event_t_USB_HOST_CLIENT_EVENT_DEV_GONE {
        log::warn!("device gone");
        SESSION_UP.store(false, Ordering::SeqCst);
        if !DEV.is_null() {
            usb_host_interface_release(CLIENT, DEV, 0);
            usb_host_device_close(CLIENT, DEV);
            DEV = std::ptr::null_mut();
        }
    }
}

/// Walk a config descriptor's raw bytes, yielding (type, offset) pairs.
unsafe fn descriptors(cfg: *const usb_config_desc_t) -> impl Iterator<Item = (u8, *mut u8)> {
    let total = (*cfg).__bindgen_anon_1.wTotalLength as usize;
    let base = cfg as *mut u8;
    let mut off = 0usize;
    core::iter::from_fn(move || {
        while off + 2 <= total {
            let p = base.add(off);
            let len = *p as usize;
            if len < 2 || off + len > total {
                return None;
            }
            let ty = *p.add(1);
            off += len;
            return Some((ty, p));
        }
        None
    })
}

unsafe fn attach() {
    usb_host_transfer_alloc(EP1_BUFSIZE, 0, core::ptr::addr_of_mut!(EP1_IN));
    usb_host_transfer_alloc(EP1_BUFSIZE, 0, core::ptr::addr_of_mut!(EP1_OUT));
    usb_host_transfer_alloc(512, 0, core::ptr::addr_of_mut!(EP4_IN));
    usb_host_transfer_alloc(520, 0, core::ptr::addr_of_mut!(EP8_OUT));
    usb_host_transfer_alloc(8, 0, core::ptr::addr_of_mut!(CTRL));

    let t = &mut *EP1_IN;
    t.device_handle = DEV;
    t.bEndpointAddress = mk1::EP_COMMAND_IN;
    t.num_bytes = EP1_BUFSIZE as i32;
    t.callback = Some(ep1_in_cb);

    let t = &mut *EP1_OUT;
    t.device_handle = DEV;
    t.bEndpointAddress = mk1::EP_COMMAND_OUT;
    t.callback = Some(ep1_out_cb);

    let t = &mut *EP4_IN;
    t.device_handle = DEV;
    t.bEndpointAddress = mk1::EP_PADS_IN;
    t.num_bytes = 512;
    t.callback = Some(ep4_in_cb);

    let t = &mut *EP8_OUT;
    t.device_handle = DEV;
    t.bEndpointAddress = mk1::EP_DISPLAY_OUT;
    t.callback = Some(ep8_out_cb);

    // Full-speed descriptor surgery (see module docs).
    let mut cfg: *const usb_config_desc_t = std::ptr::null();
    let mut fs_truncated = false;
    if usb_host_get_active_config_descriptor(DEV, &mut cfg) == ESP_OK {
        let mut eps: [*mut u8; 4] = [std::ptr::null_mut(); 4];
        let mut n_eps = 0;
        let mut ifd: *mut u8 = std::ptr::null_mut();
        for (ty, p) in descriptors(cfg) {
            match ty as u32 {
                USB_B_DESCRIPTOR_TYPE_INTERFACE => {
                    if ifd.is_null() {
                        ifd = p;
                    }
                }
                USB_B_DESCRIPTOR_TYPE_ENDPOINT => {
                    // wMaxPacketSize at offset 4, little-endian
                    let mps = u16::from_le_bytes([*p.add(4), *p.add(5)]);
                    if mps > 64 {
                        log::warn!("clamping EP {:#04x} MPS {} -> 64", *p.add(2), mps);
                        *p.add(4) = 64;
                        *p.add(5) = 0;
                    }
                    if n_eps < 4 {
                        eps[n_eps] = p;
                        n_eps += 1;
                    }
                }
                _ => {}
            }
        }
        fs_truncated = n_eps == 2 && !ifd.is_null();
        if fs_truncated {
            if usb_host_interface_claim(CLIENT, DEV, 0, 0) != ESP_OK {
                log::error!("claim alt 0 failed");
                return;
            }
            *ifd.add(3) = 1; // bAlternateSetting
            for &ep in &eps[..2] {
                // bEndpointAddress at offset 2
                *ep.add(2) = if *ep.add(2) & 0x80 != 0 {
                    mk1::EP_PADS_IN
                } else {
                    mk1::EP_DISPLAY_OUT
                };
            }
            if usb_host_interface_claim(CLIENT, DEV, 0, 1) == ESP_OK {
                log::info!("pad/display EPs claimed via synthesized alt 1");
            } else {
                log::error!("synthesized alt 1 claim failed (pads/displays dead)");
            }
        }
    }
    if !fs_truncated {
        // High-speed host (not the S3) — the real alt 1 exists.
        if usb_host_interface_claim(CLIENT, DEV, 0, 1) != ESP_OK {
            log::error!("claim alt 1 failed");
            return;
        }
    }

    // SET_INTERFACE(0, alt 1) — claim alone doesn't send it.
    let t = &mut *CTRL;
    let setup: [u8; 8] = [0x01, 0x0b, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    core::ptr::copy_nonoverlapping(setup.as_ptr(), t.data_buffer, 8);
    t.num_bytes = 8;
    t.device_handle = DEV;
    t.bEndpointAddress = 0;
    t.callback = Some(ctrl_cb);
    esp!(usb_host_transfer_submit_control(CLIENT, CTRL)).unwrap();
}

/// Blocking single-message write on EP 0x08 from the display thread.
fn ep8_write(msg: &[u8], rx: &mpsc::Receiver<bool>) -> Result<(), ()> {
    while rx.try_recv().is_ok() {} // drop any stale completion
    unsafe {
        let t = &mut *EP8_OUT;
        core::ptr::copy_nonoverlapping(msg.as_ptr(), t.data_buffer, msg.len());
        t.num_bytes = msg.len() as i32;
        if usb_host_transfer_submit(EP8_OUT) != ESP_OK {
            return Err(());
        }
    }
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(true) => Ok(()),
        _ => Err(()),
    }
}

fn display_test(rx: &mpsc::Receiver<bool>) {
    // The device stalls EP1 while blitting — send display traffic last.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut disp = mk1::display::Display::new();
    for (screen, invert) in [(0u8, false), (1u8, true)] {
        let ok = mk1::display::init_display(
            screen,
            |m| ep8_write(m, rx),
            |ms| std::thread::sleep(std::time::Duration::from_millis(ms as u64)),
        );
        if ok.is_err() {
            log::error!("display {screen} init failed");
            return;
        }
        for y in 0..mk1::display::HEIGHT {
            for x in 0..mk1::display::WIDTH {
                let g = (x * 31 / (mk1::display::WIDTH - 1)) as u8;
                disp.set_pixel(x, y, if invert { 31 - g } else { g });
            }
        }
        match disp.send_frame_fs(screen, |m| ep8_write(m, rx)) {
            Ok(()) => log::info!("display {screen}: test gradient sent"),
            Err(()) => log::error!("display {screen}: frame send failed"),
        }
    }
}

pub fn start() -> anyhow::Result<()> {
    unsafe {
        let host_cfg = usb_host_config_t {
            intr_flags: ESP_INTR_FLAG_LEVEL1 as i32,
            ..Default::default()
        };
        esp!(usb_host_install(&host_cfg))?;
    }

    std::thread::Builder::new()
        .name("usbh".into())
        .stack_size(4096)
        .spawn(|| loop {
            let mut flags = 0u32;
            unsafe {
                usb_host_lib_handle_events(u32::MAX, &mut flags);
                if flags & USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS != 0 {
                    usb_host_device_free_all();
                }
            }
        })?;

    let (tx, rx) = mpsc::sync_channel(1);
    EP8_DONE.set(tx).ok();
    std::thread::Builder::new()
        .name("mk1-disp".into())
        .stack_size(16384)
        .spawn(move || loop {
            while !SESSION_UP.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            display_test(&rx);
            // once per session; wait for a re-attach
            while SESSION_UP.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })?;

    std::thread::Builder::new()
        .name("mk1-usb".into())
        .stack_size(8192)
        .spawn(|| unsafe {
            let client_cfg = usb_host_client_config_t {
                is_synchronous: false,
                max_num_event_msg: 8,
                __bindgen_anon_1: usb_host_client_config_t__bindgen_ty_1 {
                    async_: usb_host_client_config_t__bindgen_ty_1__bindgen_ty_1 {
                        client_event_callback: Some(client_cb),
                        callback_arg: std::ptr::null_mut(),
                    },
                },
            };
            esp!(usb_host_client_register(
                &client_cfg,
                core::ptr::addr_of_mut!(CLIENT)
            ))
            .unwrap();
            log::info!("USB host ready — waiting for the Maschine Mk1");
            loop {
                // 10ms tick: EP 0x08 completions dispatch from here, and the
                // display blit is one blocking write per message.
                usb_host_client_handle_events(CLIENT, 10);
                let addr = NEW_ADDR.swap(0, Ordering::SeqCst);
                if addr != 0 && DEV.is_null() {
                    if usb_host_device_open(CLIENT, addr, core::ptr::addr_of_mut!(DEV)) != ESP_OK {
                        continue;
                    }
                    let mut dd: *const usb_device_desc_t = std::ptr::null();
                    usb_host_get_device_descriptor(DEV, &mut dd);
                    let (vid, pid) = (
                        (*dd).__bindgen_anon_1.idVendor,
                        (*dd).__bindgen_anon_1.idProduct,
                    );
                    log::info!("device {vid:04x}:{pid:04x} attached");
                    if vid == mk1::VENDOR_ID && pid == mk1::PRODUCT_ID {
                        attach();
                    } else {
                        log::info!("not a Maschine Mk1, ignoring");
                        usb_host_device_close(CLIENT, DEV);
                        DEV = std::ptr::null_mut();
                    }
                }
            }
        })?;

    Ok(())
}
