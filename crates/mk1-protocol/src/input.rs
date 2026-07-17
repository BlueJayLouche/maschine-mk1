//! Input parsing: pads (EP 0x84), knobs and buttons (EP 0x81 reports).

use crate::{CMD_MIDI_READ, CMD_READ_ERP, CMD_READ_IO};

/// Decode one endless-rotary-pot taper pair to an absolute position 0–999.
/// Same algorithm as the kernel driver (two tapers, 90° phase shifted,
/// peaks at −7 and 268).
pub fn decode_erp(a: u8, b: u8) -> u16 {
    const LOW_PEAK: i32 = -7;
    const HIGH_PEAK: i32 = 268;
    const RANGE: i32 = HIGH_PEAK - LOW_PEAK; // DEG180
    const DEG90: i32 = RANGE / 2;
    const DEG270: i32 = DEG90 + RANGE;
    const DEG360: i32 = RANGE * 2;
    let (a, b) = (a as i32, b as i32);
    let mid_value = (HIGH_PEAK + LOW_PEAK) / 2;

    let weight_b = ((mid_value - a).abs() - (RANGE / 2 - 100) / 2).clamp(0, 100);
    let weight_a = 100 - weight_b;

    let pos_b = if a < mid_value {
        // 0..90 and 270..360 degrees
        let p = b - LOW_PEAK + DEG270;
        if p >= DEG360 {
            p - DEG360
        } else {
            p
        }
    } else {
        // 90..270 degrees
        HIGH_PEAK - b + DEG90
    };

    let pos_a = if b > mid_value {
        a - LOW_PEAK
    } else {
        HIGH_PEAK - a + RANGE
    };

    let mut ret = (pos_a * weight_a + pos_b * weight_b) * 10 / DEG360;
    if ret < 0 {
        ret += 1000;
    }
    if ret >= 1000 {
        ret -= 1000;
    }
    ret as u16
}

/// The 11 knobs, in report order 1–8 (screens, left to right) then the three
/// master encoders. `Knob::ALL[i]` and `KnobValues[i]` use this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    Knob1,
    Knob2,
    Knob3,
    Knob4,
    Knob5,
    Knob6,
    Knob7,
    Knob8,
    Volume,
    Tempo,
    Swing,
}

impl Knob {
    pub const ALL: [Knob; 11] = [
        Knob::Knob1,
        Knob::Knob2,
        Knob::Knob3,
        Knob::Knob4,
        Knob::Knob5,
        Knob::Knob6,
        Knob::Knob7,
        Knob::Knob8,
        Knob::Volume,
        Knob::Tempo,
        Knob::Swing,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Knob::Knob1 => "knob/1",
            Knob::Knob2 => "knob/2",
            Knob::Knob3 => "knob/3",
            Knob::Knob4 => "knob/4",
            Knob::Knob5 => "knob/5",
            Knob::Knob6 => "knob/6",
            Knob::Knob7 => "knob/7",
            Knob::Knob8 => "knob/8",
            Knob::Volume => "volume",
            Knob::Tempo => "tempo",
            Knob::Swing => "swing",
        }
    }
}

/// (a, b) byte offsets into the ERP payload for each knob, `Knob::ALL` order.
const ERP_OFFSETS: [(usize, usize); 11] = [
    (21, 20),
    (15, 14),
    (9, 8),
    (3, 2),
    (19, 18),
    (13, 12),
    (7, 6),
    (1, 0),
    (17, 16), // volume
    (11, 10), // tempo
    (5, 4),   // swing
];

/// Absolute knob positions, 0–999 each, `Knob::ALL` order.
pub fn parse_erp(payload: &[u8]) -> Option<[u16; 11]> {
    if payload.len() < 22 {
        return None;
    }
    let mut out = [0u16; 11];
    for (i, &(a, b)) in ERP_OFFSETS.iter().enumerate() {
        out[i] = decode_erp(payload[a], payload[b]);
    }
    Some(out)
}

/// Buttons by READ_IO bit index. Discriminant = bit position in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Button {
    Mute = 0, Solo = 1, Select = 2, Duplicate = 3, Navigate = 4,
    PadMode = 5, Pattern = 6, Scene = 7,
    // bit 8 is reserved
    Rec = 9, Erase = 10, Shift = 11, Grid = 12, StepRight = 13,
    StepLeft = 14, Restart = 15,
    GroupE = 16, GroupF = 17, GroupG = 18, GroupH = 19,
    GroupD = 20, GroupC = 21, GroupB = 22, GroupA = 23,
    Control = 24, Browse = 25, Left = 26, Snap = 27, AutoWrite = 28,
    Right = 29, Sampling = 30, Step = 31,
    Softkey1 = 32, Softkey2 = 33, Softkey3 = 34, Softkey4 = 35,
    Softkey5 = 36, Softkey6 = 37, Softkey7 = 38, Softkey8 = 39,
    NoteRepeat = 40, Play = 41,
}

impl Button {
    #[rustfmt::skip]
    pub const ALL: [Button; 41] = [
        Button::Mute, Button::Solo, Button::Select, Button::Duplicate,
        Button::Navigate, Button::PadMode, Button::Pattern, Button::Scene,
        Button::Rec, Button::Erase, Button::Shift, Button::Grid,
        Button::StepRight, Button::StepLeft, Button::Restart,
        Button::GroupE, Button::GroupF, Button::GroupG, Button::GroupH,
        Button::GroupD, Button::GroupC, Button::GroupB, Button::GroupA,
        Button::Control, Button::Browse, Button::Left, Button::Snap,
        Button::AutoWrite, Button::Right, Button::Sampling, Button::Step,
        Button::Softkey1, Button::Softkey2, Button::Softkey3, Button::Softkey4,
        Button::Softkey5, Button::Softkey6, Button::Softkey7, Button::Softkey8,
        Button::NoteRepeat, Button::Play,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Button::Mute => "mute",
            Button::Solo => "solo",
            Button::Select => "select",
            Button::Duplicate => "duplicate",
            Button::Navigate => "navigate",
            Button::PadMode => "pad_mode",
            Button::Pattern => "pattern",
            Button::Scene => "scene",
            Button::Rec => "rec",
            Button::Erase => "erase",
            Button::Shift => "shift",
            Button::Grid => "grid",
            Button::StepRight => "step_right",
            Button::StepLeft => "step_left",
            Button::Restart => "restart",
            Button::GroupA => "group_a",
            Button::GroupB => "group_b",
            Button::GroupC => "group_c",
            Button::GroupD => "group_d",
            Button::GroupE => "group_e",
            Button::GroupF => "group_f",
            Button::GroupG => "group_g",
            Button::GroupH => "group_h",
            Button::Control => "control",
            Button::Browse => "browse",
            Button::Left => "left",
            Button::Snap => "snap",
            Button::AutoWrite => "autowrite",
            Button::Right => "right",
            Button::Sampling => "sampling",
            Button::Step => "step",
            Button::Softkey1 => "softkey1",
            Button::Softkey2 => "softkey2",
            Button::Softkey3 => "softkey3",
            Button::Softkey4 => "softkey4",
            Button::Softkey5 => "softkey5",
            Button::Softkey6 => "softkey6",
            Button::Softkey7 => "softkey7",
            Button::Softkey8 => "softkey8",
            Button::NoteRepeat => "note_repeat",
            Button::Play => "play",
        }
    }
}

/// Snapshot of all button states from one READ_IO report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ButtonStates(pub u64);

impl ButtonStates {
    pub fn parse(payload: &[u8]) -> Self {
        let mut bits = 0u64;
        for (i, &byte) in payload.iter().take(6).enumerate() {
            bits |= (byte as u64) << (i * 8);
        }
        ButtonStates(bits)
    }

    pub fn pressed(self, b: Button) -> bool {
        self.0 & (1 << (b as u64)) != 0
    }

    /// Buttons whose state differs from `prev`, with the new state.
    pub fn diff(self, prev: Self) -> impl Iterator<Item = (Button, bool)> {
        let changed = self.0 ^ prev.0;
        Button::ALL
            .iter()
            .copied()
            .filter(move |b| changed & (1 << (*b as u64)) != 0)
            .map(move |b| (b, self.pressed(b)))
    }
}

/// One parsed EP 0x81 message.
#[derive(Debug, PartialEq, Eq)]
pub enum Ep1Message<'a> {
    DeviceInfo(crate::DeviceInfo),
    Knobs([u16; 11]),
    Buttons(ButtonStates),
    Midi { port: u8, data: &'a [u8] },
    Unknown { cmd: u8, payload: &'a [u8] },
}

impl<'a> Ep1Message<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        let (&cmd, payload) = buf.split_first()?;
        Some(match cmd {
            crate::CMD_GET_DEVICE_INFO => Ep1Message::DeviceInfo(crate::DeviceInfo::parse(payload)?),
            CMD_READ_ERP => Ep1Message::Knobs(parse_erp(payload)?),
            CMD_READ_IO => Ep1Message::Buttons(ButtonStates::parse(payload)),
            CMD_MIDI_READ => {
                let (&port, rest) = payload.split_first()?;
                let (&len, data) = rest.split_first()?;
                Ep1Message::Midi {
                    port,
                    data: data.get(..len as usize)?,
                }
            }
            _ => Ep1Message::Unknown { cmd, payload },
        })
    }
}

/// Parse an EP 0x84 pad report: yields (pad 0–15, pressure 0–4095).
/// Words are self-identifying; the same pad may appear more than once.
pub fn parse_pads(buf: &[u8]) -> impl Iterator<Item = (u8, u16)> + '_ {
    buf.chunks_exact(2).map(|c| {
        let v = u16::from_le_bytes([c[0], c[1]]);
        ((v >> 12) as u8, v & 0xfff)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erp_matches_kernel_reference() {
        // Hand-computed against the C algorithm: a at mid value, b above mid.
        assert_eq!(decode_erp(130, 200), 249);
        for a in 0..=255u8 {
            for b in [0u8, 60, 130, 200, 255] {
                assert!(decode_erp(a, b) < 1000);
            }
        }
    }

    #[test]
    fn pads_parse() {
        // pad 3 at pressure 0x123, pad 15 at 0xfff
        let buf = [0x23, 0x31, 0xff, 0xff];
        let v: Vec<_> = parse_pads(&buf).collect();
        assert_eq!(v, [(3, 0x123), (15, 0xfff)]);
    }

    #[test]
    fn buttons_diff() {
        let prev = ButtonStates(0);
        let mut payload = [0u8; 6];
        payload[0] = 0x01; // bit 0 = mute
        payload[5] = 0x02; // bit 41 = play
        let now = ButtonStates::parse(&payload);
        assert!(now.pressed(Button::Mute) && now.pressed(Button::Play));
        let d: Vec<_> = now.diff(prev).collect();
        assert_eq!(d, [(Button::Mute, true), (Button::Play, true)]);
    }

    #[test]
    fn knob_order_matches_offsets() {
        // knob1 reads bytes (21,20); put a distinctive pair there.
        let mut payload = [0u8; 22];
        payload[21] = 130;
        payload[20] = 200;
        let knobs = parse_erp(&payload).unwrap();
        assert_eq!(knobs[0], decode_erp(130, 200));
    }
}
