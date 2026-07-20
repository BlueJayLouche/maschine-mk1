//! WiFi OSC engine — profile-mapped controls out, LEDs/feedback in
//! (docs/design.md "OSC surface", "Banking", "LEDs", "Profiles").
//!
//! Controls go to the active profile's target (default: device config) at
//! banked addresses `/maschine/<a-h>/pad|knob|softkey/<n>`, globals at
//! `/maschine/volume|tempo|swing|button/<name>`, all floats 0–1 — unless the
//! profile maps a control elsewhere (e.g. straight to `/rustjay/*`, which is
//! what retired tools/rustjay-shim.py). Group buttons are device-owned bank
//! selectors; shift+group switches profiles.
//!
//! Inbound on :9001: `/maschine/led/*` writes (banked or legacy), profile
//! `led_source` feedback, and `/maschine/ping` → `/maschine/pong` echo (see
//! tools/osc-rtt.py).

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use mk1_protocol as mk1;
use mk1::input::Button;
use mk1::leds::Led;

use crate::profile::{self, Profile};

pub enum Event {
    /// Knob index into `Knob::ALL` (0–7 = knobs, 8–10 = volume/tempo/swing), raw 0–999.
    Knob(usize, u16),
    /// Raw pad id 0–15, pressure 0–4095.
    Pad(u8, u16),
    Button(Button, bool),
    /// OSC message received on :9001 (LED writes + led_source feedback).
    In(String, f32),
    /// Profile storage or active slot changed (web page) — reload.
    Reload,
}

static TX: OnceLock<mpsc::SyncSender<Event>> = OnceLock::new();

/// Called from the USB client context and the web server; drops events if
/// the engine is backed up or not started (portal mode).
pub fn publish(e: Event) {
    if let Some(tx) = TX.get() {
        let _ = tx.try_send(e);
    }
}

fn pad4(b: &mut Vec<u8>) {
    b.push(0);
    while b.len() % 4 != 0 {
        b.push(0);
    }
}

fn osc_head(addr: &str, tag: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(addr.len() + 12);
    b.extend_from_slice(addr.as_bytes());
    pad4(&mut b);
    b.extend_from_slice(tag.as_bytes());
    pad4(&mut b);
    b
}

fn osc_msg(addr: &str, v: f32) -> Vec<u8> {
    let mut b = osc_head(addr, ",f");
    b.extend_from_slice(&v.to_be_bytes());
    b
}

fn osc_msg_i(addr: &str, v: i32) -> Vec<u8> {
    let mut b = osc_head(addr, ",i");
    b.extend_from_slice(&v.to_be_bytes());
    b
}

fn osc_msg_s(addr: &str, s: &str) -> Vec<u8> {
    let mut b = osc_head(addr, ",s");
    b.extend_from_slice(s.as_bytes());
    pad4(&mut b);
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

fn bank_letter(b: usize) -> char {
    (b'a' + b as u8) as char
}

fn group_index(b: Button) -> Option<usize> {
    use Button as B;
    Some(match b {
        B::GroupA => 0,
        B::GroupB => 1,
        B::GroupC => 2,
        B::GroupD => 3,
        B::GroupE => 4,
        B::GroupF => 5,
        B::GroupG => 6,
        B::GroupH => 7,
        _ => return None,
    })
}

const GROUP_LEDS: [Led; 8] = [
    Led::GroupA, Led::GroupB, Led::GroupC, Led::GroupD,
    Led::GroupE, Led::GroupF, Led::GroupG, Led::GroupH,
];

const SOFTKEY_LEDS: [Led; 8] = [
    Led::Softkey1, Led::Softkey2, Led::Softkey3, Led::Softkey4,
    Led::Softkey5, Led::Softkey6, Led::Softkey7, Led::Softkey8,
];

fn softkey_index(b: Button) -> Option<usize> {
    use Button as B;
    Some(match b {
        B::Softkey1 => 0,
        B::Softkey2 => 1,
        B::Softkey3 => 2,
        B::Softkey4 => 3,
        B::Softkey5 => 4,
        B::Softkey6 => 5,
        B::Softkey7 => 6,
        B::Softkey8 => 7,
        _ => return None,
    })
}

/// An LED a feedback value can land on. Banked kinds carry (bank, index).
#[derive(Clone, Copy)]
enum LedSlot {
    Pad(usize, usize),
    Softkey(usize, usize),
    Button(Led),
}

/// Parse a control key ("a/pad/3", "b/softkey/1", "button/rec") into the LED
/// it owns. Also parses the banked `/maschine/led/` write names, which share
/// the syntax. Group LEDs are device-owned; softkeys are addressed banked.
fn led_slot(key: &str) -> Option<LedSlot> {
    if let Some(name) = key.strip_prefix("button/") {
        let b = *Button::ALL.iter().find(|b| b.name() == name)?;
        if group_index(b).is_some() || softkey_index(b).is_some() {
            return None;
        }
        return Led::for_button(b).map(LedSlot::Button);
    }
    let mut it = key.splitn(3, '/');
    let bank = it.next()?.as_bytes();
    let bank = match bank {
        [c @ b'a'..=b'h'] => (c - b'a') as usize,
        _ => return None,
    };
    let kind = it.next()?;
    let n: usize = it.next()?.parse().ok()?;
    match kind {
        "pad" if (1..=16).contains(&n) => Some(LedSlot::Pad(bank, n - 1)),
        "softkey" if (1..=8).contains(&n) => Some(LedSlot::Softkey(bank, n - 1)),
        _ => None,
    }
}

struct Engine {
    sock: UdpSocket,
    default_target: SocketAddr,
    target: SocketAddr,
    profile: Profile,
    slot: u8,
    bank: usize,
    shift: bool,
    /// Control ("MIDI" on the silkscreen) held — the profile-switch modifier.
    ctrl: bool,
    nvs: EspDefaultNvsPartition,
    led_tx: mpsc::SyncSender<(Led, u8)>,
    /// Last raw 0–999 per physical knob 1–8, for delta integration.
    prev_raw: [Option<u16>; 8],
    /// Virtual knob positions [bank][knob], 0–1 (design.md "Knobs").
    virt: [[f32; 8]; 8],
    /// Banked host LED state, repainted on bank switch (design.md "LEDs").
    pad_led: [[u8; 16]; 8],
    softkey_led: [[u8; 8]; 8],
    /// led_source address → LED, rebuilt on profile load.
    feedback: HashMap<String, LedSlot>,
}

impl Engine {
    fn emit(&self, key: String, v: f32) {
        let addr = match self.profile.map.get(&key) {
            Some(e) => match &e.osc {
                Some(a) => a.clone(),
                None => return, // mapped entry without an address = silenced
            },
            None => format!("/maschine/{key}"),
        };
        let _ = self.sock.send_to(&osc_msg(&addr, v), self.target);
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::Knob(i, raw) => {
                if i < 8 {
                    let d = match self.prev_raw[i].replace(raw) {
                        // shortest way around the 0–999 wrap
                        Some(p) => {
                            let mut d = raw as i32 - p as i32;
                            if d > 500 {
                                d -= 1000;
                            } else if d < -500 {
                                d += 1000;
                            }
                            d
                        }
                        None => 0,
                    };
                    let key = format!("{}/knob/{}", bank_letter(self.bank), i + 1);
                    let e = self.profile.map.get(&key);
                    let scale = e.and_then(|e| e.scale).unwrap_or(1.0) as f32;
                    // mode "wrap": wrapping position, so delta-deriving hosts
                    // (vp404 trim) keep endless travel — the clamped virtual
                    // position dies at its rails.
                    let wrap = matches!(e.and_then(|e| e.mode.as_deref()), Some("wrap"));
                    let v = &mut self.virt[self.bank][i];
                    *v = *v + d as f32 * scale / 999.0;
                    *v = if wrap { v.rem_euclid(1.0) } else { v.clamp(0.0, 1.0) };
                    let v = *v;
                    self.emit(key, v);
                } else {
                    let name = ["volume", "tempo", "swing"][i - 8];
                    self.emit(name.to_string(), raw as f32 / 999.0);
                }
            }
            Event::Pad(raw, p) => {
                let n = mk1::input::pad_number(raw);
                self.emit(
                    format!("{}/pad/{}", bank_letter(self.bank), n),
                    p as f32 / 4095.0,
                );
            }
            Event::Button(b, down) => self.on_button(b, down),
            Event::In(addr, v) => self.on_in(&addr, v),
            Event::Reload => self.reload(),
        }
    }

    fn on_button(&mut self, b: Button, down: bool) {
        if b == Button::Shift || b == Button::Control {
            if b == Button::Shift {
                self.shift = down;
            } else {
                self.ctrl = down;
            }
            self.paint_groups();
        }
        if let Some(g) = group_index(b) {
            // Bare group presses are inert — far too easy to hit live.
            // Shift+group = bank, MIDI(Control)+group = profile: two-finger
            // chords both; shift wins if both modifiers are down.
            if down {
                if self.shift {
                    self.set_bank(g);
                } else if self.ctrl {
                    self.set_slot(g as u8);
                }
            }
            return; // never forwarded — reserved bank/profile selectors
        }
        if let Some(n) = softkey_index(b) {
            self.emit(
                format!("{}/softkey/{}", bank_letter(self.bank), n + 1),
                down as u8 as f32,
            );
            return;
        }
        self.emit(format!("button/{}", b.name()), down as u8 as f32);
    }

    fn set_bank(&mut self, g: usize) {
        if self.bank == g {
            return;
        }
        self.bank = g;
        log::info!("bank {}", bank_letter(g));
        let _ = self
            .sock
            .send_to(&osc_msg_s("/maschine/bank", &bank_letter(g).to_string()), self.target);
        self.paint_groups();
        for i in 0..16 {
            let _ = self.led_tx.try_send((Led::pad(i), self.pad_led[g][i]));
        }
        for i in 0..8 {
            let _ = self.led_tx.try_send((SOFTKEY_LEDS[i], self.softkey_led[g][i]));
        }
    }

    fn set_slot(&mut self, s: u8) {
        if let Err(e) = profile::set_active(self.nvs.clone(), s) {
            log::warn!("persisting active profile failed: {e}");
        }
        self.reload();
    }

    fn reload(&mut self) {
        self.slot = profile::active(self.nvs.clone());
        self.profile = profile::load(self.nvs.clone(), self.slot).unwrap_or_default();
        self.target = self
            .profile
            .target
            .as_deref()
            .and_then(|t| t.parse().ok())
            .unwrap_or(self.default_target);
        self.feedback = self
            .profile
            .map
            .iter()
            .filter_map(|(key, e)| Some((e.led_source.clone()?, led_slot(key)?)))
            .collect();
        self.paint_groups();
        self.sync_now();
        log::info!(
            "profile {} \"{}\" active, target {}, {} mapped controls",
            self.slot,
            self.profile.name,
            self.target,
            self.profile.map.len()
        );
    }

    fn sync_now(&self) {
        if let Some(addr) = &self.profile.sync {
            let _ = self.sock.send_to(&osc_msg_i(addr, 9001), self.target);
        }
    }

    /// Group LEDs: active bank normally, active profile while MIDI (the
    /// profile-switch modifier) is held.
    fn paint_groups(&self) {
        let lit = if self.ctrl && !self.shift {
            self.slot as usize
        } else {
            self.bank
        };
        for g in 0..8 {
            let _ = self
                .led_tx
                .try_send((GROUP_LEDS[g], if g == lit { 63 } else { 0 }));
        }
    }

    fn on_in(&mut self, addr: &str, v: f32) {
        let b = (v.clamp(0.0, 1.0) * 63.0) as u8;
        if let Some(name) = addr.strip_prefix("/maschine/led/") {
            if let Some(slot) = self.led_name_slot(name) {
                self.apply_led(slot, b);
            }
            return;
        }
        if let Some(&slot) = self.feedback.get(addr) {
            self.apply_led(slot, b);
        }
    }

    /// `/maschine/led/` names: banked ("a/pad/3"), legacy unbanked ("pad/3",
    /// "softkey3", button names → active bank), and "backlight".
    fn led_name_slot(&self, name: &str) -> Option<LedSlot> {
        if let Some(slot) = led_slot(name) {
            return Some(slot);
        }
        if name == "backlight" {
            return Some(LedSlot::Button(Led::Backlight));
        }
        if let Some(n) = name.strip_prefix("pad/") {
            let n: usize = n.parse().ok().filter(|n| (1..=16).contains(n))?;
            return Some(LedSlot::Pad(self.bank, n - 1));
        }
        let b = *Button::ALL.iter().find(|b| b.name() == name)?;
        if group_index(b).is_some() {
            return None;
        }
        if let Some(i) = softkey_index(b) {
            return Some(LedSlot::Softkey(self.bank, i));
        }
        Led::for_button(b).map(LedSlot::Button)
    }

    fn apply_led(&mut self, slot: LedSlot, b: u8) {
        match slot {
            LedSlot::Pad(bank, i) => {
                self.pad_led[bank][i] = b;
                if bank == self.bank {
                    let _ = self.led_tx.try_send((Led::pad(i), b));
                }
            }
            LedSlot::Softkey(bank, i) => {
                self.softkey_led[bank][i] = b;
                if bank == self.bank {
                    let _ = self.led_tx.try_send((SOFTKEY_LEDS[i], b));
                }
            }
            LedSlot::Button(led) => {
                let _ = self.led_tx.try_send((led, b));
            }
        }
    }
}

pub fn start(
    cfg: &crate::config::Config,
    nvs: EspDefaultNvsPartition,
    led_tx: mpsc::SyncSender<(Led, u8)>,
) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Event>(64);
    TX.set(tx).ok();
    let default_target: SocketAddr =
        format!("{}:{}", cfg.target_ip, cfg.target_port).parse()?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    log::info!("OSC engine up: controls -> {default_target}, LEDs/feedback <- :9001");

    let mut engine = Engine {
        sock,
        default_target,
        target: default_target,
        profile: Profile::default(),
        slot: 0,
        bank: 0,
        shift: false,
        ctrl: false,
        nvs,
        led_tx,
        prev_raw: [None; 8],
        virt: [[0.0; 8]; 8],
        pad_led: [[0; 16]; 8],
        softkey_led: [[0; 8]; 8],
        feedback: HashMap::new(),
    };

    std::thread::Builder::new()
        .name("osc".into())
        .stack_size(8192)
        .spawn(move || {
            engine.reload();
            let mut last_sync = Instant::now();
            loop {
                if last_sync.elapsed() >= Duration::from_secs(5) {
                    engine.sync_now();
                    last_sync = Instant::now();
                }
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(e) => engine.on_event(e),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;

    std::thread::Builder::new()
        .name("osc-in".into())
        .stack_size(4096)
        .spawn(move || {
            let s = match UdpSocket::bind("0.0.0.0:9001") {
                Ok(s) => s,
                Err(e) => {
                    log::error!("OSC in port bind failed: {e}");
                    return;
                }
            };
            let mut buf = [0u8; 512];
            loop {
                let Ok((n, peer)) = s.recv_from(&mut buf) else {
                    continue;
                };
                let Some((addr, val)) = osc_parse(&buf[..n]) else {
                    continue;
                };
                // RTT probe answered right here — no engine hop in the number.
                if addr == "/maschine/ping" {
                    let _ = s.send_to(&osc_msg("/maschine/pong", val), peer);
                    continue;
                }
                publish(Event::In(addr.to_string(), val));
            }
        })?;
    Ok(())
}
