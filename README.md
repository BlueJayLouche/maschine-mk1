# maschine-mk1

Rescue firmware & drivers for the original Native Instruments **Maschine Mk1**
controller (USB `17cc:0808`, 2009). NI's software dropped it years ago — the
hardware is excellent: 16 pressure pads, 11 endless knobs, 41 backlit buttons,
and two 255×64 **32-level grayscale** displays. This project keeps those units
out of landfill by turning them into standalone wireless OSC/MIDI controllers.

## Status

- **works**: standalone ESP32-S3 firmware (`firmware-rs/`) running the full
  device at USB full speed — pads/knobs/buttons → OSC, LEDs ← OSC + local
  echo, both displays, WLED-style AP onboarding, 8 mappable profiles with a
  web editor. The Linux bridges (Python/Rust) remain as battle-tested paths.
- **next**: screens over OSC (text + bitmap), DIN MIDI OUT from profiles,
  mDNS discovery

## Layout

| path | what |
|------|------|
| `firmware-rs/` | **the product**: esp-rs firmware for an ESP32-S3 living inside the unit — USB host session, WiFi OSC engine, profiles, captive-portal onboarding, web editor |
| `crates/mk1-protocol` | `no_std` packet codecs: input, LEDs, displays, DIN MIDI. Shared between the Linux driver and the firmware |
| `crates/mk1-bridge` | userspace libusb driver + OSC bridge (Linux) |
| `bridge-python/` | original bridge that rides the kernel `snd-usb-caiaq` driver — no custom USB code, still the most battle-tested path |
| `tools/` | bench utilities: `osc-rtt.py` latency probe, `vp404-profile.json` example profile |
| `docs/protocol.md` | the full reverse-engineered USB protocol |
| `docs/design.md` | settled design decisions for the standalone controller |

## OSC surface

The 8 group buttons are **bank selectors a–h**: pads, knobs 1–8, and softkeys
bank together (holding Shift turns them into **profile** selectors instead).

Controls out (UDP → configured target, default `:9000`), all floats 0–1:
banked `/maschine/<a-h>/pad/<1-16>` (continuous pressure while held),
`/maschine/<a-h>/knob/<1-8>` (virtual positions integrated from the endless
encoders, one per bank slot), `/maschine/<a-h>/softkey/<1-8>`; global
`/maschine/volume|tempo|swing` and `/maschine/button/<name>` (0/1);
`/maschine/bank <a-h>` broadcast on every switch.

LEDs in (UDP → device `:9001`): banked `/maschine/led/<a-h>/pad/<n>` and
`/maschine/led/<a-h>/softkey/<n>` float 0–1 — the device stores all banks and
repaints on bank switch; `/maschine/led/<button-name>` and
`/maschine/led/backlight` are global, and legacy unbanked `led/pad/<n>` writes
the active bank. The device also echoes `/maschine/ping` back as
`/maschine/pong` (that's `tools/osc-rtt.py`).

**Profiles** remap any control per slot: 8 sparse JSON profiles, edited at
`http://<device-ip>/` once on your WiFi, switched from the hardware with
Shift+group. A map entry is `{osc, led_source, label}` — `osc` replaces the
generated address outright (see `tools/vp404-profile.json`, which points bank
a straight at [rustjay-engine](https://github.com/BlueJayLouche/rustjay-engine)'s
`/rustjay/*` params with loaded-pad LED feedback via `led_source`), and a
profile-level `sync` address registers the device for host feedback pushes.
Unmapped controls keep their generated `/maschine/*` address; the device
speaks plain OSC to anything.

## Running (ESP32-S3)

```sh
. "$HOME/export-esp.sh"   # esp-rs Xtensa toolchain env
cd firmware-rs && cargo build
espflash flash target/xtensa-esp32s3-espidf/debug/mk1-firmware --port <port>
```

Plug the Mk1 into the S3's USB-OTG port. First boot opens a WPA2 AP
`maschine-XXXX` (password `maschine`) with a captive portal for WiFi
credentials and the OSC target; once joined, the profiles editor lives at
`http://<device-ip>/`. Holding Shift+MIDI through boot forces the portal back
open.

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

## Latency

Measured on the ESP32-S3 firmware over 2.4 GHz WiFi (`tools/osc-rtt.py`,
which times a `/maschine/ping` → `/maschine/pong` echo against the device's
:9001): **RTT median 7.3 ms, min 6.2 ms** across 300 pings. One-way is
therefore ≈3.7 ms; pad-hit → OSC-at-host adds the Mk1's own pad scan cadence
(2–10 ms, fixed in the device) on top: **≈6–14 ms total**. WiFi power save is
disabled in firmware (the esp-idf default adds 30–300 ms jitter).

The tail (p95 ~60 ms in a lived-in apartment) is not the controller: the
identical tail shows up pinging the router from the host with no controller
involved, and correlates with macOS AWDL (AirDrop) channel-hopping. For tight
sessions: `sudo ifconfig awdl0 down` on the Mac, or put the host on Ethernet.
`osc-rtt.py` is the regression guard — run it before/after firmware changes.

For note-critical timing the wired path is the unit's own rear DIN MIDI OUT;
WiFi OSC pad timing is best-effort by design.

## Credits

Protocol knowledge owes everything to the Linux `snd-usb-caiaq` driver by
Daniel Mack (GPL-2.0, read for facts, no code copied) and
[cabl](https://github.com/shaduzlabs/cabl) by Vincenzo Pacella (MIT), verified
against real hardware. See also [maschine.rs](https://github.com/wrl/maschine.rs)
for the Mikro Mk2 generation.

## License

MIT or Apache-2.0, at your option.
