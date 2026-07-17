//! Userspace driver + OSC bridge for the Maschine Mk1.
//!
//! Detaches the kernel snd-usb-caiaq driver, drives the device raw, and
//! bridges to/from OSC with the same surface as the Python kernel bridge:
//! controls out to <target>:9000, LEDs in on :9001 (/maschine/led/<name>).
//!
//! Usage: mk1-bridge [target-ip] [--dump]

use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mk1_protocol as mk1;
use mk1::display::Display;
use mk1::input::{Button, ButtonStates, Ep1Message};
use mk1::leds::{Led, LedState, MAX_BRIGHTNESS};
use rusb::{DeviceHandle, GlobalContext};

const OSC_OUT_PORT: u16 = 9000;
const OSC_IN_PORT: u16 = 9001;
const USB_TIMEOUT: Duration = Duration::from_millis(250);

struct Mk1 {
    handle: DeviceHandle<GlobalContext>,
}

impl Mk1 {
    fn open() -> rusb::Result<Self> {
        let handle = rusb::open_device_with_vid_pid(mk1::VENDOR_ID, mk1::PRODUCT_ID)
            .ok_or(rusb::Error::NoDevice)?;
        handle.set_auto_detach_kernel_driver(true)?;
        handle.claim_interface(mk1::INTERFACE)?;
        handle.set_alternate_setting(mk1::INTERFACE, mk1::ALT_SETTING)?;
        Ok(Mk1 { handle })
    }

    fn write_ep1(&self, msg: &[u8]) -> rusb::Result<()> {
        self.handle.write_bulk(mk1::EP_COMMAND_OUT, msg, USB_TIMEOUT)?;
        Ok(())
    }

    fn write_display(&self, msg: &[u8]) -> rusb::Result<()> {
        self.handle.write_bulk(mk1::EP_DISPLAY_OUT, msg, USB_TIMEOUT)?;
        Ok(())
    }

    fn handshake(&self) -> rusb::Result<mk1::DeviceInfo> {
        self.write_ep1(&mk1::get_device_info())?;
        let mut buf = [0u8; mk1::EP1_BUFSIZE];
        for _ in 0..20 {
            let n = match self.handle.read_bulk(mk1::EP_COMMAND_IN, &mut buf, USB_TIMEOUT) {
                Ok(n) => n,
                Err(rusb::Error::Timeout) => continue,
                Err(e) => return Err(e),
            };
            if let Some(Ep1Message::DeviceInfo(info)) = Ep1Message::parse(&buf[..n]) {
                return Ok(info);
            }
        }
        Err(rusb::Error::Timeout)
    }
}

fn osc(addr: &str, v: f32) -> Vec<u8> {
    let pad = |b: &mut Vec<u8>| b.extend(std::iter::repeat(0).take(4 - b.len() % 4));
    let mut b = addr.as_bytes().to_vec();
    pad(&mut b);
    b.extend(b",f");
    pad(&mut b);
    b.extend(v.to_be_bytes());
    b
}

// ponytail: single-float messages only — all this bridge ever receives
fn osc_decode(data: &[u8]) -> Option<(&str, f32)> {
    let addr = std::str::from_utf8(data.split(|&c| c == 0).next()?).ok()?;
    let v = f32::from_be_bytes(data.get(data.len() - 4..)?.try_into().ok()?);
    Some((addr, v))
}

fn led_by_name(name: &str) -> Option<Led> {
    if let Some(n) = name.strip_prefix("pad/") {
        let n: usize = n.parse().ok()?;
        return (1..=16).contains(&n).then(|| Led::pad(n - 1));
    }
    if name == "backlight" {
        return Some(Led::Backlight);
    }
    Button::ALL
        .iter()
        .find(|b| b.name() == name)
        .and_then(|&b| Led::for_button(b))
}

fn flush_leds(dev: &Mk1, leds: &mut LedState) {
    for frame in leds.frames() {
        if let Err(e) = dev.write_ep1(&frame) {
            eprintln!("LED write failed: {e}");
        }
    }
}

fn test_pattern(left: &mut Display, right: &mut Display) {
    for y in 0..mk1::display::HEIGHT {
        for x in 0..mk1::display::WIDTH {
            // left: horizontal gradient, right: checkerboard
            left.set_pixel(x, y, (x * 32 / mk1::display::WIDTH) as u8);
            right.set_pixel(x, y, if (x / 8 + y / 8) % 2 == 0 { 31 } else { 4 });
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dump = args.iter().any(|a| a == "--dump");
    let target = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| "192.168.1.137".into());

    let dev = Arc::new(Mk1::open().expect("open Maschine Mk1 (is it plugged in? permissions?)"));
    let info = dev.handshake().expect("device info handshake");
    println!(
        "Maschine Mk1: firmware {}, {} MIDI in / {} out",
        info.fw_version, info.num_midi_in, info.num_midi_out
    );

    // Displays: init both, then show a test pattern.
    let (mut left, mut right) = (Display::new(), Display::new());
    test_pattern(&mut left, &mut right);
    for (i, d) in [&left, &right].into_iter().enumerate() {
        mk1::display::init_display(i as u8, |m| dev.write_display(m), |ms| {
            std::thread::sleep(Duration::from_millis(ms as u64))
        })
        .expect("display init");
        d.send_frame(i as u8, |m| dev.write_display(m)).expect("display frame");
    }
    println!("displays initialized (gradient left, checkerboard right)");

    // LEDs: hello sweep, then backlight on.
    let leds = Arc::new(Mutex::new(LedState::new()));
    {
        let mut l = leds.lock().unwrap();
        l.set_all(MAX_BRIGHTNESS);
        flush_leds(&dev, &mut l);
        std::thread::sleep(Duration::from_millis(400));
        l.set_all(0);
        l.set(Led::Backlight, 0x5c);
        flush_leds(&dev, &mut l);
    }

    // Enable input streaming.
    dev.write_ep1(&mk1::auto_msg(1, 10, 5)).expect("auto_msg");

    let sock = UdpSocket::bind("0.0.0.0:0").expect("bind OSC out socket");
    let out = move |sock: &UdpSocket, name: &str, v: f32| {
        let _ = sock.send_to(&osc(&format!("/maschine/{name}"), v), (target.as_str(), OSC_OUT_PORT));
        if dump {
            println!("{name:20} {v:.3}");
        }
    };

    // Pads: EP 0x84 stream, echo pressure to pad LEDs.
    {
        let (dev, leds, sock) = (dev.clone(), leds.clone(), sock.try_clone().unwrap());
        let out = out.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            let mut last = [0u16; 16];
            loop {
                let n = match dev.handle.read_bulk(mk1::EP_PADS_IN, &mut buf, USB_TIMEOUT) {
                    Ok(n) => n,
                    Err(rusb::Error::Timeout) => continue,
                    Err(e) => {
                        eprintln!("pad read failed: {e}");
                        break;
                    }
                };
                let mut l = leds.lock().unwrap();
                for (pad, pressure) in mk1::input::parse_pads(&buf[..n]) {
                    let pad = pad as usize % 16;
                    // kernel-style deadzone, quantized to cut event spam
                    let p = if pressure < 6 { 0 } else { pressure };
                    if p.abs_diff(last[pad]) < 32 && !(p == 0 && last[pad] != 0) {
                        continue;
                    }
                    last[pad] = p;
                    out(&sock, &format!("pad/{}", pad + 1), p as f32 / 4095.0);
                    l.set(Led::pad(pad), (p >> 6) as u8);
                }
                flush_leds(&dev, &mut l);
            }
        });
    }

    // LEDs over OSC: /maschine/led/<name> <0..1> on :9001.
    {
        let (dev, leds) = (dev.clone(), leds.clone());
        std::thread::spawn(move || {
            let sock = UdpSocket::bind(("0.0.0.0", OSC_IN_PORT)).expect("bind OSC LED port");
            let mut buf = [0u8; 512];
            while let Ok((n, _)) = sock.recv_from(&mut buf) {
                let Some((addr, v)) = osc_decode(&buf[..n]) else { continue };
                let Some(name) = addr.strip_prefix("/maschine/led/") else { continue };
                if let Some(led) = led_by_name(name) {
                    let mut l = leds.lock().unwrap();
                    l.set(led, (v.clamp(0.0, 1.0) * MAX_BRIGHTNESS as f32) as u8);
                    flush_leds(&dev, &mut l);
                }
            }
        });
    }

    // Knobs, buttons, DIN MIDI in: EP 0x81, on the main thread.
    let mut knobs: Option<[u16; 11]> = None;
    let mut buttons = ButtonStates::default();
    let mut buf = [0u8; mk1::EP1_BUFSIZE];
    loop {
        let n = match dev.handle.read_bulk(mk1::EP_COMMAND_IN, &mut buf, USB_TIMEOUT) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => continue,
            Err(e) => {
                eprintln!("EP1 read failed: {e}");
                break;
            }
        };
        match Ep1Message::parse(&buf[..n]) {
            Some(Ep1Message::Knobs(now)) => {
                let prev = knobs.unwrap_or(now);
                for (i, knob) in mk1::input::Knob::ALL.iter().enumerate() {
                    if now[i] != prev[i] || knobs.is_none() {
                        out(&sock, knob.name(), now[i] as f32 / 999.0);
                    }
                }
                knobs = Some(now);
            }
            Some(Ep1Message::Buttons(now)) => {
                let mut l = leds.lock().unwrap();
                for (b, pressed) in now.diff(buttons) {
                    out(&sock, &format!("button/{}", b.name()), pressed as u8 as f32);
                    if let Some(led) = Led::for_button(b) {
                        l.set(led, if pressed { MAX_BRIGHTNESS } else { 0 });
                    }
                }
                flush_leds(&dev, &mut l);
                buttons = now;
            }
            Some(Ep1Message::Midi { port, data }) => {
                println!("DIN MIDI in (port {port}): {data:02x?}");
            }
            _ => {}
        }
    }
}
