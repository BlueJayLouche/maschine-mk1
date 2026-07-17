//! Protocol codecs for the NI Maschine Mk1 controller (USB 17cc:0808).
//!
//! Pure packet encode/decode, no I/O — usable from a libusb host driver and
//! from embedded firmware alike. See `docs/protocol.md` in the repo for the
//! wire protocol itself. Protocol facts learned from the Linux `snd-usb-caiaq`
//! driver and the MIT-licensed cabl project, verified on hardware.

#![cfg_attr(not(test), no_std)]

pub mod display;
pub mod input;
pub mod leds;

pub const VENDOR_ID: u16 = 0x17cc;
pub const PRODUCT_ID: u16 = 0x0808;

/// Interface 0 must be switched to alt setting 1 before anything else.
pub const INTERFACE: u8 = 0;
pub const ALT_SETTING: u8 = 1;

pub const EP_COMMAND_OUT: u8 = 0x01;
pub const EP_COMMAND_IN: u8 = 0x81;
pub const EP_PADS_IN: u8 = 0x84;
pub const EP_DISPLAY_OUT: u8 = 0x08;

/// EP1 messages are at most this long.
pub const EP1_BUFSIZE: usize = 64;

pub const CMD_GET_DEVICE_INFO: u8 = 0x01;
pub const CMD_READ_ERP: u8 = 0x02;
pub const CMD_READ_ANALOG: u8 = 0x03;
pub const CMD_READ_IO: u8 = 0x04;
pub const CMD_WRITE_IO: u8 = 0x05;
pub const CMD_MIDI_READ: u8 = 0x06;
pub const CMD_MIDI_WRITE: u8 = 0x07;
pub const CMD_AUTO_MSG: u8 = 0x0b;
pub const CMD_DIMM_LEDS: u8 = 0x0c;

pub const fn get_device_info() -> [u8; 1] {
    [CMD_GET_DEVICE_INFO]
}

/// Enable periodic input reports. The kernel driver uses (1, 10, 5).
pub const fn auto_msg(digital: u8, analog: u8, erp: u8) -> [u8; 4] {
    [CMD_AUTO_MSG, digital, analog, erp]
}

/// Encode a DIN MIDI OUT message into `out`; returns the byte count to send.
/// Data longer than one EP1 message must be split by the caller.
pub fn midi_write(data: &[u8], out: &mut [u8; EP1_BUFSIZE]) -> usize {
    let len = data.len().min(EP1_BUFSIZE - 3);
    out[0] = CMD_MIDI_WRITE;
    out[1] = 0; // port
    out[2] = len as u8;
    out[3..3 + len].copy_from_slice(&data[..len]);
    len + 3
}

/// Reply payload of GET_DEVICE_INFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub fw_version: u16,
    pub hw_subtype: u8,
    pub num_erp: u8,
    pub num_analog_in: u8,
    pub num_digital_in: u8,
    pub num_digital_out: u8,
    pub num_midi_out: u8,
    pub num_midi_in: u8,
}

impl DeviceInfo {
    /// Parse from the reply payload (bytes after the command byte).
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 13 {
            return None;
        }
        Some(DeviceInfo {
            fw_version: u16::from_le_bytes([p[0], p[1]]),
            hw_subtype: p[2],
            num_erp: p[3],
            num_analog_in: p[4],
            num_digital_in: p[5],
            num_digital_out: p[6],
            num_midi_out: p[11],
            num_midi_in: p[12],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_write_layout() {
        let mut buf = [0u8; EP1_BUFSIZE];
        let n = midi_write(&[0x90, 60, 100], &mut buf);
        assert_eq!(&buf[..n], &[0x07, 0x00, 3, 0x90, 60, 100]);
    }

    #[test]
    fn auto_msg_layout() {
        assert_eq!(auto_msg(1, 10, 5), [0x0b, 1, 10, 5]);
    }
}
