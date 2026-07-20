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

Revised 2026-07-20 (bench: bare group presses are far too easy to hit
mid-set): a bare group press is **inert** — bank switch is **Shift+group**,
profile switch is **MIDI(Control)+group** (two-finger chords both; a
three-key chord proved unplayable). Group LEDs show the active bank, or the
active profile while MIDI is held.

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

The virtual position **clamps at its rails**, which kills relative-delta
derivation on the host side — so a profile map entry may set
`"mode": "wrap"` (2026-07-18) to send a wrapping position instead (0–1,
rollover, never clamped). That is the contract vp404's trim knobs consume
(wrap-corrected deltas, rustjay `3ac36ce`); the shim and pre-profile firmware
always sent it, and the clamped virtual briefly broke it. Map entries may
also set `"scale"` (default 1.0 = one revolution ≈ full range): the published
position advances at `delta × scale`, so e.g. 0.15 makes the minimum knob
step (5/999 of a revolution, set by the ERP jitter deadband) ≈ 1 frame on a
1200-frame clip. Sensitivity lives here, per profile — not in the host and
not in the deadband, which stays at the kernel-proven value.

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

**Latency constraint (settled 2026-07-18):** the Mk1 stalls EP1 (knob/button
reports) while a display blit is in flight, so the renderer is
partial-window-updates-only from day one: dirty cells via row/column windows,
any single burst capped at ~2 EP 0x08 messages, and display traffic yields
while input was active in the last ~100 ms. Full-frame blits are init/idle
only. Fallback if this still measurably regresses the latency baseline: a
freeze-screens-while-playing flag.

## Profiles

- **8 slots**, one JSON file each on a flash data partition; sparse per-control
  map `{osc, midi {ch, type, num}, label, led_source}` + `name`, `target`,
  flags. Missing entries use generated defaults. Active slot persists in NVS.
- `led_source` binds a control's LED to an incoming feedback address: rustjay's
  `osc-feedback` channel pushes registered param values on change (plus a full
  dump on `/rustjay/sync`), and a matching address drives the LED float 0–1.
  E.g. vp404 publishes `pad<i>_loaded` and `rec_state` params for exactly this.
- Switch: **hold MIDI(Control) + group button** (revised 2026-07-20, was
  shift+group); group LEDs show profiles while MIDI is held.
- Editing: device web page — download/upload/paste profile JSON, name + target
  as form fields. No graphical mapping editor in v1.
- Implemented 2026-07-18 (`firmware-rs`: profile.rs/osc.rs/web.rs), with two
  notes: slots live as NVS blobs (`p0`–`p7`, ≤3 KB JSON each) rather than files
  on a dedicated partition — same flash, no partition-table change; revisit if
  a profile outgrows the cap. `midi` and `label` entries are stored and served
  back but not yet emitted/rendered (DIN MIDI plumbing and screens are their
  own work items). Profile JSON also carries optional `target` ("ip:port"
  override) and `sync` (feedback registration address, sent every 5 s with the
  LED port 9001 as int arg — rustjay's `/rustjay/sync`). The web page is
  paste/download JSON per slot (name/target ride in the JSON, not form fields).
  `tools/vp404-profile.json` is the starter profile that retired the shim.

## Onboarding

Unconfigured (or STA join fails): WPA2 AP `maschine-XXXX` (password
`maschine`), DNS catch-all + captive portal serving exactly WiFi credentials +
OSC target. Configured: joins the LAN, same web server (+ profiles page);
mDNS `maschine.local` arrives with the profiles page, not the portal.

## Latency (settled 2026-07-18)

Doctrine: **protect, don't chase**. Target ≤10 ms p50 / ≤20 ms p99
controller-attributable (pad hit → OSC at the host, Mac AWDL down) — already
met: measured RTT median 7.3 ms (`tools/osc-rtt.py`, `/maschine/ping` echo on
:9001), and the tail was proven Mac-side (identical tail Mac→router with no
controller involved; fix is `sudo ifconfig awdl0 down` or Ethernet). The
regression guard is the osc-rtt RTT — the only component firmware can
regress; run it before/after nontrivial changes. Non-goals until a measured
symptom appears: core pinning, thread priorities, FreeRTOS tick rate,
AUTO_MSG rate tweaks (kernel-proven values; the budget is radio-dominated).
WiFi power save stays off (`esp_wifi_set_ps(WIFI_PS_NONE)`); no per-event
info-level logging on input paths.

## Host-side work queue (rustjay repo)

- OSC-out: push param values on change → knob-slot sync, LED writes, screen
  content.
