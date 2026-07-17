//! Display framebuffers and EP 0x08 message encoding.
//!
//! Two 255×64 panels, 32-level grayscale, 5 bits per pixel with 3 pixels
//! packed into 2 bytes. Stored values are inverted (0x1F = dark). Init and
//! frame sequences follow cabl (MIT), verbatim.

pub const WIDTH: usize = 255;
pub const HEIGHT: usize = 64;
pub const ROW_BYTES: usize = 170;
pub const BUF_SIZE: usize = ROW_BYTES * HEIGHT; // 10880

const CHUNK: usize = 502;
const LAST_CHUNK: usize = 338;

/// Command payloads (after the `[d, len_be_u16]` header) and the delay in ms
/// to wait after sending each, for panel initialization.
#[rustfmt::skip]
pub const INIT_COMMANDS: &[(&[u8], u32)] = &[
    (&[0x30], 0),
    (&[0xCA, 0x04, 0x0F, 0x00], 20),
    (&[0xBB, 0x00], 0),
    (&[0xD1], 0),
    (&[0x94], 0),
    (&[0x81, 0x1E, 0x02], 20),
    (&[0x20, 0x08], 20),
    (&[0x20, 0x0B], 20),
    (&[0xA6], 0),
    (&[0x31], 0),
    (&[0x32, 0x00, 0x00, 0x05], 0),
    (&[0x34], 0),
    (&[0x30], 0),
    (&[0xBC, 0x00, 0x01, 0x02], 0),
    (&[0x75, 0x00, 0x3F], 0),
    (&[0x15, 0x00, 0x54], 0),
    (&[0x5C], 0),
    (&[0x25], 20),
    (&[0xAF], 20),
    (&[0xBC, 0x02, 0x01, 0x01], 0),
    (&[0xA6], 0),
    (&[0x81, 0x25, 0x02], 0),
];

/// Send the init sequence for `display` (0 or 1). `write` sends one message
/// on EP 0x08; `sleep_ms` must actually wait.
pub fn init_display<E>(
    display: u8,
    mut write: impl FnMut(&[u8]) -> Result<(), E>,
    mut sleep_ms: impl FnMut(u32),
) -> Result<(), E> {
    let d = display << 1;
    let mut msg = [0u8; 8];
    for &(cmd, delay) in INIT_COMMANDS {
        msg[0] = d;
        msg[1] = 0x00;
        msg[2] = cmd.len() as u8;
        msg[3..3 + cmd.len()].copy_from_slice(cmd);
        write(&msg[..3 + cmd.len()])?;
        if delay > 0 {
            sleep_ms(delay);
        }
    }
    Ok(())
}

/// One display's framebuffer.
#[derive(Clone)]
pub struct Display {
    buf: [u8; BUF_SIZE],
}

impl Default for Display {
    fn default() -> Self {
        Self::new()
    }
}

impl Display {
    /// Starts black.
    pub fn new() -> Self {
        Display {
            buf: [0xff; BUF_SIZE],
        }
    }

    /// gray: 0 = dark, 31 = fully lit.
    pub fn set_pixel(&mut self, x: usize, y: usize, gray: u8) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let v = 31 - gray.min(31); // stored inverted
        let i = y * ROW_BYTES + (x / 3) * 2;
        match x % 3 {
            0 => self.buf[i] = (self.buf[i] & 0x07) | (v << 3),
            1 => {
                self.buf[i] = (self.buf[i] & 0xF8) | (v >> 2);
                self.buf[i + 1] = (self.buf[i + 1] & 0x3F) | (v << 6);
            }
            _ => self.buf[i + 1] = (self.buf[i + 1] & 0xE0) | v,
        }
    }

    pub fn fill(&mut self, gray: u8) {
        let v = 31 - gray.min(31);
        for i in (0..BUF_SIZE).step_by(2) {
            self.buf[i] = (v << 3) | (v >> 2);
            self.buf[i + 1] = (v << 6) | v;
        }
    }

    pub fn raw(&self) -> &[u8; BUF_SIZE] {
        &self.buf
    }

    pub fn raw_mut(&mut self) -> &mut [u8; BUF_SIZE] {
        &mut self.buf
    }

    /// Send the whole framebuffer to `display` (0 or 1) as EP 0x08 messages.
    pub fn send_frame<E>(
        &self,
        display: u8,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let d = display << 1;
        write(&[d, 0x00, 0x03, 0x75, 0x00, 0x3F])?; // row window 0–63
        write(&[d, 0x00, 0x03, 0x15, 0x00, 0x54])?; // col window 0–84 (×3 px)

        let mut msg = [0u8; 4 + CHUNK];

        // first chunk carries the write-data command byte 0x5C
        msg[..4].copy_from_slice(&[d, 0x01, 0xF7, 0x5C]);
        msg[4..4 + CHUNK].copy_from_slice(&self.buf[..CHUNK]);
        write(&msg[..4 + CHUNK])?;

        let mut offset = CHUNK;
        while offset + CHUNK <= BUF_SIZE - LAST_CHUNK {
            msg[..3].copy_from_slice(&[d | 1, 0x01, 0xF6]);
            msg[3..3 + CHUNK].copy_from_slice(&self.buf[offset..offset + CHUNK]);
            write(&msg[..3 + CHUNK])?;
            offset += CHUNK;
        }

        msg[..3].copy_from_slice(&[d | 1, 0x01, 0x52]);
        msg[3..3 + LAST_CHUNK].copy_from_slice(&self.buf[offset..]);
        write(&msg[..3 + LAST_CHUNK])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_packing() {
        let mut disp = Display::new();
        // full brightness → stored 0
        disp.set_pixel(0, 0, 31);
        disp.set_pixel(1, 0, 31);
        disp.set_pixel(2, 0, 31);
        assert_eq!(&disp.raw()[..2], &[0x00, 0x20]); // only the unused bit set

        // known mid value in each slot
        let mut disp = Display::new();
        disp.set_pixel(3, 0, 21); // stored 10 = 0b01010, slot 0 → bits 7..3
        assert_eq!(disp.raw()[2], (10 << 3) | 0x07);
        disp.set_pixel(4, 0, 21); // slot 1: high 3 bits then low 2
        assert_eq!(disp.raw()[2] & 0x07, 10 >> 2);
        assert_eq!(disp.raw()[3] & 0xC0, (10u8 << 6) & 0xC0);
        disp.set_pixel(5, 0, 21); // slot 2 → bits 4..0
        assert_eq!(disp.raw()[3] & 0x1F, 10);
    }

    #[test]
    fn frame_chunking() {
        let disp = Display::new();
        let mut sizes = Vec::new();
        let mut total = 0usize;
        disp.send_frame(1, |m| {
            sizes.push((m[0], m.len()));
            if m.len() > 6 {
                total += m.len() - if sizes.len() == 3 { 4 } else { 3 };
            }
            Ok::<(), ()>(())
        })
        .unwrap();
        assert_eq!(sizes.len(), 2 + 22);
        assert_eq!(sizes[2], (2, 4 + CHUNK)); // d = display<<1
        assert_eq!(sizes[3], (3, 3 + CHUNK)); // continuation d|1
        assert_eq!(sizes.last(), Some(&(3, 3 + LAST_CHUNK)));
        assert_eq!(total, BUF_SIZE);
    }

    #[test]
    fn init_sequence_shape() {
        let mut count = 0;
        let mut slept = 0;
        init_display(1, |m| {
            assert_eq!(m[0], 2);
            assert_eq!(m[2] as usize, m.len() - 3);
            count += 1;
            Ok::<(), ()>(())
        }, |ms| slept += ms)
        .unwrap();
        assert_eq!(count, INIT_COMMANDS.len());
        assert_eq!(slept, 120);
    }
}
