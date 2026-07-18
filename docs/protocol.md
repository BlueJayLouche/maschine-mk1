# Maschine Mk1 USB protocol

The original NI Maschine controller (USB `17cc:0808`) predates NI's HID protocol.
It speaks a vendor-specific protocol shared with other caiaq-era devices.

Facts below were learned by reading the Linux kernel driver
(`sound/usb/caiaq`, GPL-2.0 — read for protocol facts only, no code copied) and
[cabl](https://github.com/shaduzlabs/cabl) (MIT) — see `reference/` (not
committed) — and verified against real hardware. Where the two sources disagree
(LED positions, MIDI write header), the kernel is authoritative: its table was
verified LED-by-LED against a real unit.

## Endpoints (interface 0, alt setting 1)

| EP | Dir | Purpose |
|------|-----|---------|
| 0x01 | OUT | command channel (64-byte max messages) |
| 0x81 | IN  | command replies + knob/button/MIDI-in reports |
| 0x84 | IN  | pad pressure stream (512-byte reports) |
| 0x08 | OUT | displays |

Session start: `set_interface(0, alt=1)`, start reading EP 0x81, send
`GET_DEVICE_INFO`, wait for the reply, then enable input streaming with
`AUTO_MSG`.

## EP1 commands

Message = `[cmd, payload…]`, ≤ 64 bytes. Replies on EP 0x81 echo the command
byte first.

| cmd | name | payload |
|------|------|---------|
| 0x01 | GET_DEVICE_INFO | none; reply: fw_version u16le, hw_subtype, num_erp, num_analog_in, num_digital_in, num_digital_out, … data_alignment (13 bytes) |
| 0x02 | READ_ERP | (reply/report) 22 bytes: 11 knobs as (a,b) taper pairs, see below |
| 0x04 | READ_IO | (reply/report) button bitfield, bit *i* = `BUTTONS[i]`, 42 bits |
| 0x06 | MIDI_READ | (report) `[0x06, port, len, data…]` — DIN MIDI IN |
| 0x07 | MIDI_WRITE | `[0x07, port=0, len, data…]` — DIN MIDI OUT |
| 0x0b | AUTO_MSG | `[0x0b, digital, analog, erp]` — report rates; kernel uses (1, 10, 5) |
| 0x0c | DIMM_LEDS | `[0x0c, bank, 32 brightness bytes]`, see LEDs |

## Pads (EP 0x84)

Reports are a sequence of little-endian u16 words (≥ 32 bytes):
`word >> 12` = raw pad id 0–15, `word & 0xfff` = pressure 0–4095.
Pads are self-identifying; don't assume position. **Raw ids run row-major from
the top-left; the numbers printed on the unit start at the bottom-left**
(raw 0 = printed 13, raw 12 = printed 1 — verified on hardware).
Kernel deadzone: 5.
cabl ignores reports whose first byte is 2 (collision with encoder turns) and
uses a press threshold of 200/4095.

## Knobs (ERP payload byte offsets, after cmd byte)

Endless rotary pots, two 90°-shifted tapers per knob, decoded to absolute
0–999 (`decode_erp` — see kernel `input.c` / `mk1-protocol/src/input.rs`;
peaks −7/268).

| knob | (a, b) | | knob | (a, b) |
|------|--------|-|------|--------|
| 1 (left screen) | 21, 20 | | 5 (right screen) | 19, 18 |
| 2 | 15, 14 | | 6 | 13, 12 |
| 3 | 9, 8   | | 7 | 7, 6 |
| 4 | 3, 2   | | 8 | 1, 0 |
| volume | 17, 16 | | tempo | 11, 10 |
| swing  | 5, 4   | | | |

## Buttons (READ_IO bit order)

0 mute, 1 solo, 2 select, 3 duplicate, 4 navigate, 5 pad_mode, 6 pattern,
7 scene, 8 (reserved), 9 rec, 10 erase, 11 shift, 12 grid, 13 step_right,
14 step_left, 15 restart, 16–23 group E F G H D C B A, 24 control, 25 browse,
26 left, 27 snap, 28 autowrite, 29 right, 30 sampling, 31 step,
32–39 softkey **8–1** (right-to-left, verified on hardware), 40 note_repeat,
41 play.

## LEDs

62-position brightness array (0–63 each), written in two banks over EP1:
`[0x0c, 0x00, state[0..32]]` and `[0x0c, 0x1e, state[32..64]]`.

Positions: pads 1–16 → 3 2 1 0 / 7 6 5 4 / 11 10 9 8 / 15 14 13 12 (each row
of four reversed); mute 16, solo 17, select 18, duplicate 19, navigate 20,
pad_mode 21, pattern 22, scene 23, shift 24, erase 25, grid 26, step_right 27,
rec 28, play 29, step_left 32, restart 33, groups A–H → 41 40 37 36 39 38 35
34, autowrite 42, snap 43, right 44, left 45, sampling 46, browse 47, step 48,
control 49, softkeys 1–8 → 57 56 55 54 53 52 51 50, note_repeat 58,
display backlight 59.

## Displays (EP 0x08)

Two 255×64 panels, 32-level grayscale. Every message is
`[d, len_be_u16, payload…]` where `d` = display×2 for a message that starts a
command, `d|1` for raw-data continuation.

**Framebuffer**: 10880 bytes = 64 rows × 170 bytes. 3 pixels pack into 2 bytes,
5 bits each: `[p0:7..3 | p1:2..0] [p1:7..6 | x | p2:4..0]`. Stored values are
**inverted** (0x1F = dark, 0x00 = fully lit).

**Init** (per display, from cabl, verbatim): send the command payloads below
with the listed delays after them:
`30` · `CA 04 0F 00` (+20 ms) · `BB 00` · `D1` · `94` · `81 1E 02` (+20 ms) ·
`20 08` (+20 ms) · `20 0B` (+20 ms) · `A6` · `31` · `32 00 00 05` · `34` · `30`
· `BC 00 01 02` · `75 00 3F` · `15 00 54` · `5C` · `25` (+20 ms) · `AF`
(+20 ms) · `BC 02 01 01` · `A6` · `81 25 02`.

**Frame**: `75 00 3F` (row window 0–63), `15 00 54` (column window 0–84,
85 × 3 px = 255), then the buffer as: `[d, 0x01, 0xF7, 0x5C, data[0..502]]`,
20 × `[d|1, 0x01, 0xF6, data[n..n+502]]`, `[d|1, 0x01, 0x52, data[10542..10880]]`.

## Full-speed behaviour (hardware-verified on ESP32-S3, 2026-07-18)

The Mk1 enumerates on a full-speed-only host but serves a **truncated,
spec-illegal config descriptor**: one interface, alt setting 0 only (no alt 1),
just EP 0x01/0x81 — and both still claim `wMaxPacketSize 512`, illegal at FS.
NI never implemented other-speed descriptors. In reality:

- Clamp the host-side MPS to 64 (the FS wire maximum) and EP1 works fine —
  EP1 messages are ≤ 64 bytes anyway.
- `SET_INTERFACE(0, alt 1)` is **accepted** despite alt 1 being undescribed,
  and the full EP1 session runs: GET_DEVICE_INFO, AUTO_MSG, knob/button
  reports, LEDs.
- EP 0x84 (pads) and EP 0x08 (displays) are absent from the FS descriptor;
  whether the device still serves them on the wire is untested — a host-stack
  patch is needed to open pipes on undescribed endpoints.

## Power

The unit is USB bus-powered; budget ≥ 500 mA at 5 V with backlight and LEDs lit.
