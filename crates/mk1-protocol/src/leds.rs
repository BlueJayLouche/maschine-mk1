//! LED state and DIMM_LEDS frame encoding.
//!
//! 62 physical positions, brightness 0–63, written as two 32-byte banks.
//! Positions follow the kernel driver's table (verified on hardware).

use crate::CMD_DIMM_LEDS;

/// Physical LED positions in the brightness array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Led {
    Pad4 = 0, Pad3 = 1, Pad2 = 2, Pad1 = 3,
    Pad8 = 4, Pad7 = 5, Pad6 = 6, Pad5 = 7,
    Pad12 = 8, Pad11 = 9, Pad10 = 10, Pad9 = 11,
    Pad16 = 12, Pad15 = 13, Pad14 = 14, Pad13 = 15,
    Mute = 16, Solo = 17, Select = 18, Duplicate = 19, Navigate = 20,
    PadMode = 21, Pattern = 22, Scene = 23, Shift = 24, Erase = 25,
    Grid = 26, StepRight = 27, Rec = 28, Play = 29,
    StepLeft = 32, Restart = 33,
    GroupH = 34, GroupG = 35, GroupD = 36, GroupC = 37,
    GroupF = 38, GroupE = 39, GroupB = 40, GroupA = 41,
    AutoWrite = 42, Snap = 43, Right = 44, Left = 45,
    Sampling = 46, Browse = 47, Step = 48, Control = 49,
    Softkey8 = 50, Softkey7 = 51, Softkey6 = 52, Softkey5 = 53,
    Softkey4 = 54, Softkey3 = 55, Softkey2 = 56, Softkey1 = 57,
    NoteRepeat = 58, Backlight = 59,
}

impl Led {
    /// LED for pad index 0–15 (pad 1 = index 0).
    pub fn pad(index: usize) -> Led {
        const PADS: [Led; 16] = [
            Led::Pad1, Led::Pad2, Led::Pad3, Led::Pad4,
            Led::Pad5, Led::Pad6, Led::Pad7, Led::Pad8,
            Led::Pad9, Led::Pad10, Led::Pad11, Led::Pad12,
            Led::Pad13, Led::Pad14, Led::Pad15, Led::Pad16,
        ];
        PADS[index % 16]
    }

    /// LED behind a button, if it has one.
    pub fn for_button(b: crate::input::Button) -> Option<Led> {
        use crate::input::Button as B;
        Some(match b {
            B::Mute => Led::Mute,
            B::Solo => Led::Solo,
            B::Select => Led::Select,
            B::Duplicate => Led::Duplicate,
            B::Navigate => Led::Navigate,
            B::PadMode => Led::PadMode,
            B::Pattern => Led::Pattern,
            B::Scene => Led::Scene,
            B::Rec => Led::Rec,
            B::Erase => Led::Erase,
            B::Shift => Led::Shift,
            B::Grid => Led::Grid,
            B::StepRight => Led::StepRight,
            B::StepLeft => Led::StepLeft,
            B::Restart => Led::Restart,
            B::GroupA => Led::GroupA,
            B::GroupB => Led::GroupB,
            B::GroupC => Led::GroupC,
            B::GroupD => Led::GroupD,
            B::GroupE => Led::GroupE,
            B::GroupF => Led::GroupF,
            B::GroupG => Led::GroupG,
            B::GroupH => Led::GroupH,
            B::Control => Led::Control,
            B::Browse => Led::Browse,
            B::Left => Led::Left,
            B::Snap => Led::Snap,
            B::AutoWrite => Led::AutoWrite,
            B::Right => Led::Right,
            B::Sampling => Led::Sampling,
            B::Step => Led::Step,
            B::Softkey1 => Led::Softkey1,
            B::Softkey2 => Led::Softkey2,
            B::Softkey3 => Led::Softkey3,
            B::Softkey4 => Led::Softkey4,
            B::Softkey5 => Led::Softkey5,
            B::Softkey6 => Led::Softkey6,
            B::Softkey7 => Led::Softkey7,
            B::Softkey8 => Led::Softkey8,
            B::NoteRepeat => Led::NoteRepeat,
            B::Play => Led::Play,
        })
    }
}

pub const MAX_BRIGHTNESS: u8 = 63;
const BANK_SIZE: usize = 32;

/// Full LED state with dirty tracking per bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedState {
    state: [u8; 64],
    dirty: [bool; 2],
}

impl Default for LedState {
    fn default() -> Self {
        Self::new()
    }
}

impl LedState {
    pub const fn new() -> Self {
        LedState {
            state: [0; 64],
            dirty: [true; 2], // force initial sync
        }
    }

    pub fn set(&mut self, led: Led, brightness: u8) {
        let pos = led as usize;
        let v = brightness.min(MAX_BRIGHTNESS);
        if self.state[pos] != v {
            self.state[pos] = v;
            self.dirty[pos / BANK_SIZE] = true;
        }
    }

    pub fn set_all(&mut self, brightness: u8) {
        for pos in 0..=Led::Backlight as usize {
            self.state[pos] = brightness.min(MAX_BRIGHTNESS);
        }
        self.dirty = [true; 2];
    }

    /// Encode pending changes as EP1 DIMM_LEDS messages and clear dirty flags.
    pub fn frames(&mut self) -> impl Iterator<Item = [u8; 2 + BANK_SIZE]> {
        const BANK_IDS: [u8; 2] = [0x00, 0x1e];
        let mut out = [None; 2];
        for bank in 0..2 {
            if core::mem::take(&mut self.dirty[bank]) {
                let mut msg = [0u8; 2 + BANK_SIZE];
                msg[0] = CMD_DIMM_LEDS;
                msg[1] = BANK_IDS[bank];
                msg[2..].copy_from_slice(&self.state[bank * BANK_SIZE..(bank + 1) * BANK_SIZE]);
                out[bank] = Some(msg);
            }
        }
        out.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_only_dirty_banks() {
        let mut leds = LedState::new();
        assert_eq!(leds.frames().count(), 2); // initial sync
        assert_eq!(leds.frames().count(), 0);

        leds.set(Led::Play, 63);
        let frames: Vec<_> = leds.frames().collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], 0x0c);
        assert_eq!(frames[0][1], 0x00);
        assert_eq!(frames[0][2 + Led::Play as usize], 63);

        leds.set(Led::Backlight, 40);
        let frames: Vec<_> = leds.frames().collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][1], 0x1e);
        assert_eq!(frames[0][2 + (Led::Backlight as usize - 32)], 40);
    }

    #[test]
    fn pad_led_row_reversal() {
        assert_eq!(Led::pad(0), Led::Pad1);
        assert_eq!(Led::pad(0) as usize, 3);
        assert_eq!(Led::pad(4) as usize, 7);
        assert_eq!(Led::pad(15) as usize, 12);
    }
}
