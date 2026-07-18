//! WiFi OSC bridge — Pi-bridge parity (docs/design.md profiles come later).
//!
//! Controls out to `<target>:port` as `/maschine/pad/1..16`, `/maschine/knob/1..8`,
//! `/maschine/volume|tempo|swing`, `/maschine/button/<name>` (floats 0–1).
//! LEDs in on `:9001` as `/maschine/led/<name>` (pad/1..16, button names,
//! backlight; float 0–1). Stdlib OSC, single-float messages — same wire
//! format as bridge-python and tools/rustjay-shim.py.

use std::net::UdpSocket;
use std::sync::mpsc;
use std::sync::OnceLock;

use mk1_protocol as mk1;

pub enum Event {
    /// Knob index into `Knob::ALL` (0–7 = knobs, 8–10 = volume/tempo/swing), raw 0–999.
    Knob(usize, u16),
    /// Raw pad id 0–15, pressure 0–4095.
    Pad(u8, u16),
    Button(mk1::input::Button, bool),
}

static TX: OnceLock<mpsc::SyncSender<Event>> = OnceLock::new();

/// Called from the USB client context; drops events if the bridge is
/// backed up or not started (portal mode).
pub fn publish(e: Event) {
    if let Some(tx) = TX.get() {
        let _ = tx.try_send(e);
    }
}

fn osc_msg(addr: &str, v: f32) -> Vec<u8> {
    let mut b = Vec::with_capacity(addr.len() + 12);
    b.extend_from_slice(addr.as_bytes());
    b.push(0);
    while b.len() % 4 != 0 {
        b.push(0);
    }
    b.extend_from_slice(b",f\0\0");
    b.extend_from_slice(&v.to_be_bytes());
    b
}

fn osc_parse(d: &[u8]) -> Option<(&str, f32)> {
    let end = d.iter().position(|&b| b == 0)?;
    let addr = core::str::from_utf8(&d[..end]).ok()?;
    if d.len() < end + 8 {
        return None;
    }
    let v = f32::from_be_bytes(d[d.len() - 4..].try_into().ok()?);
    Some((addr, v))
}

fn led_by_name(name: &str) -> Option<mk1::leds::Led> {
    if let Some(num) = name.strip_prefix("pad/") {
        return num
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=16).contains(n))
            .map(|n| mk1::leds::Led::pad(n - 1));
    }
    if name == "backlight" {
        return Some(mk1::leds::Led::Backlight);
    }
    mk1::input::Button::ALL
        .iter()
        .find(|b| b.name() == name)
        .and_then(|&b| mk1::leds::Led::for_button(b))
}

pub fn start(
    target_ip: &str,
    target_port: u16,
    led_tx: mpsc::SyncSender<(mk1::leds::Led, u8)>,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Event>(64);
    TX.set(tx).ok();
    let target: std::net::SocketAddr = format!("{target_ip}:{target_port}").parse()?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    log::info!("OSC bridge up: controls -> {target}, LEDs <- :9001");

    std::thread::Builder::new()
        .name("osc-out".into())
        .stack_size(4096)
        .spawn(move || {
            for e in rx {
                let (addr, v) = match e {
                    Event::Knob(i, raw) => {
                        let name = match i {
                            0..=7 => format!("/maschine/knob/{}", i + 1),
                            8 => "/maschine/volume".into(),
                            9 => "/maschine/tempo".into(),
                            _ => "/maschine/swing".into(),
                        };
                        (name, raw as f32 / 999.0)
                    }
                    Event::Pad(raw, p) => (
                        format!("/maschine/pad/{}", mk1::input::pad_number(raw)),
                        p as f32 / 4095.0,
                    ),
                    Event::Button(b, down) => (
                        format!("/maschine/button/{}", b.name()),
                        down as u8 as f32,
                    ),
                };
                let _ = sock.send_to(&osc_msg(&addr, v), target);
            }
        })?;

    std::thread::Builder::new()
        .name("osc-led".into())
        .stack_size(4096)
        .spawn(move || {
            let s = match UdpSocket::bind("0.0.0.0:9001") {
                Ok(s) => s,
                Err(e) => {
                    log::error!("LED port bind failed: {e}");
                    return;
                }
            };
            let mut buf = [0u8; 256];
            loop {
                let Ok((n, _)) = s.recv_from(&mut buf) else {
                    continue;
                };
                let Some((addr, val)) = osc_parse(&buf[..n]) else {
                    continue;
                };
                let Some(name) = addr.strip_prefix("/maschine/led/") else {
                    continue;
                };
                if let Some(led) = led_by_name(name) {
                    let _ = led_tx.try_send((led, (val.clamp(0.0, 1.0) * 63.0) as u8));
                }
            }
        })?;
    Ok(())
}
