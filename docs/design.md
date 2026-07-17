# Standalone controller design

Settled 2026-07-17. These are decisions, not proposals — re-open one only with a
reason the original reasoning didn't cover. Protocol facts live in
[protocol.md](protocol.md).

## Transports

- **OSC over WiFi** to a configured target (default `:9000`), values normalized
  0–1 floats.
- **DIN MIDI OUT** on the Mk1's rear jack, driven over the same USB protocol
  (`CMD_MIDI_WRITE`) — the instrument-grade path. Tested against Bitwig.
- No USB-MIDI device mode, no network MIDI. Both transports are always live:
  a control emits OSC if it has an address, MIDI if it has a map, both if both.

## Banking

The 8 group buttons are **reserved** bank selectors; group = one global page
switching pads + knobs 1–8 + softkeys together. Volume/tempo/swing and the
labeled buttons stay global. Bank = MIDI channel 1–8.

Logical control surface per profile: **128 pad slots, 64 knob slots,
64 softkey slots, ~25 global buttons, 3 global knobs.**

## OSC surface

- Generated default addresses: banked controls `/maschine/<a-h>/pad/<1-16>`,
  `/maschine/<a-h>/knob/<1-8>`, `/maschine/<a-h>/softkey/<1-8>`; globals as
  today (`/maschine/volume`, `/maschine/button/play`, …).
- Any slot may carry a **per-control address override** in the profile
  (rustjay-engine matches exact `/rustjay/<category>/<id>` addresses and
  normalizes 0–1 internally — no rustjay changes needed).
- Pads stream continuous pressure while held — that is the "aftertouch" story
  for OSC consumers (ISF shader params etc.).
- Device broadcasts `/maschine/bank <a-h>` on bank switch.

## Knobs

Endless. The device integrates deltas into a **virtual 0–1 position per knob
slot** (RAM only, starts at 0; one physical revolution ≈ full range) and sends
absolute values. Known ceiling: positions go stale if the host changes a param
elsewhere; the fix is **rustjay OSC-out** (planned host-side work, also the
channel for LED/screen feedback), not device rework.

## MIDI map defaults (per-control overridable)

| Control | Default |
|---|---|
| Pads | Note On/Off 36–51 (printed order), ch = bank, velocity from pressure attack peak |
| Knobs 1–8 | Relative CC 16–23 (bin-offset 64±delta), ch = bank |
| Softkeys | CC 102–109, 127/0, ch = bank |
| Volume / tempo / swing | CC 7 / 14 / 15, ch 1 fixed |
| Global buttons | CC 110+, ch 1 fixed |

No aftertouch in v1 (per-profile flag when a use appears — Bitwig-side pressure
is the trigger, OSC never needs it).

## LEDs

Device holds **banked LED state**: 128 pad + 64 softkey slots. Hosts write
`/maschine/led/<a-h>/pad/<n>` (float 0–1) on `:9001`; the device paints the
active bank and repaints on every switch (e.g. vp404 lights loaded-sample pads
once, correct across banks forever). Unbanked legacy addresses write the active
bank. Group LEDs are device-owned bank/profile indicators. Global button LEDs
unbanked. Pad pressure→LED local echo is a profile flag, default on.
Future, not v1: DIN MIDI IN notes → pad LEDs.

## Screens

Local UI per screen: **4 cells** aligned with the knobs beneath (label from
profile + value bar + value), softkey label strip along the top edge; left
status line `profile · bank`, right status line WiFi/target. Turning a knob
zooms its cell (last-touched overlay, ~1 s decay). Host takeover is per-screen
via the text/bitmap OSC protocol until released or profile change; bank
switches flash a brief bank-letter overlay even over host content.

## Profiles

- **8 slots**, one JSON file each on a flash data partition; sparse per-control
  map `{osc, midi {ch, type, num}, label}` + `name`, `target`, flags. Missing
  entries use generated defaults. Active slot persists in NVS.
- Switch: **hold shift + group button**; group LEDs show profiles while shift
  is held.
- Editing: device web page — download/upload/paste profile JSON, name + target
  as form fields. No graphical mapping editor in v1.

## Onboarding

Unconfigured (or STA join fails): WPA2 AP `maschine-XXXX` (password
`maschine`), DNS catch-all + captive portal serving exactly WiFi credentials +
OSC target. Configured: joins the LAN, same web server (+ profiles page);
mDNS `maschine.local` arrives with the profiles page, not the portal.

## Host-side work queue (rustjay repo)

- OSC-out: push param values on change → knob-slot sync, LED writes, screen
  content.
