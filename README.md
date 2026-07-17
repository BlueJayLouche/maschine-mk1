# maschine-mk1

Rescue firmware & drivers for the original Native Instruments **Maschine Mk1**
controller (USB `17cc:0808`, 2009). NI's software dropped it years ago — the
hardware is excellent: 16 pressure pads, 11 endless knobs, 41 backlit buttons,
and two 255×64 **32-level grayscale** displays. This project keeps those units
out of landfill by turning them into standalone wireless OSC/MIDI controllers.

## Status

- **works**: pads/knobs/buttons → OSC, LEDs ← OSC, LED echo, display test
  patterns — via either the kernel-driver bridge (Python) or the userspace
  libusb driver (Rust)
- **next**: text + bitmap OSC protocol for the displays, then ESP32-S3
  firmware (WLED-style AP onboarding) so the brain lives inside the unit

## Layout

| path | what |
|------|------|
| `crates/mk1-protocol` | `no_std` packet codecs: input, LEDs, displays, DIN MIDI. Shared between the Linux driver and (later) ESP32-S3 firmware |
| `crates/mk1-bridge` | userspace libusb driver + OSC bridge (Linux) |
| `bridge-python/` | original bridge that rides the kernel `snd-usb-caiaq` driver — no custom USB code, still the most battle-tested path |
| `docs/protocol.md` | the full reverse-engineered USB protocol |

## OSC surface

Controls out (UDP → `target:9000`): `/maschine/pad/1..16` (0–1 pressure),
`/maschine/knob/1..8`, `/maschine/volume|tempo|swing` (0–1),
`/maschine/button/<name>` (0/1).

LEDs in (UDP → device `:9001`): `/maschine/led/<name>` float 0–1, where name is
`pad/1..16`, any button name, or `backlight`.

Designed for [rustjay-engine](https://github.com/BlueJayLouche/rustjay-engine)
(OSC params on :9000 by default) but speaks plain OSC to anything.

## Running (Linux)

```sh
cargo build --release -p mk1-bridge
sudo ./target/release/mk1-bridge <target-ip> --dump   # sudo or a udev rule
```

The bridge detaches the kernel driver while it runs. To hand the device back
to `snd-usb-caiaq` afterwards, a plain rebind is often not enough — the
firmware wedges if a session died mid-command. Known-good recovery (or just
replug the cable):

```sh
sudo python -c "import fcntl,os; fcntl.ioctl(os.open('/dev/bus/usb/001/%03d' \
  % int(open('/sys/bus/usb/devices/<port>/devnum').read()), os.O_WRONLY), 21780, 0)"
sudo modprobe -r snd_usb_caiaq && sudo modprobe snd_usb_caiaq
``` Wired instrument-grade output
is available through the unit's own rear DIN MIDI jacks — pad timing over WiFi
OSC is best-effort.

## Credits

Protocol knowledge owes everything to the Linux `snd-usb-caiaq` driver by
Daniel Mack (GPL-2.0, read for facts, no code copied) and
[cabl](https://github.com/shaduzlabs/cabl) by Vincenzo Pacella (MIT), verified
against real hardware. See also [maschine.rs](https://github.com/wrl/maschine.rs)
for the Mikro Mk2 generation.

## License

MIT or Apache-2.0, at your option.
