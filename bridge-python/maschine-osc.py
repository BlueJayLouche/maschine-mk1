#!/usr/bin/env python3
"""Maschine Mk1 (snd-usb-caiaq) <-> OSC bridge for rustjay-engine.

Out: pads/knobs/buttons -> OSC floats to <target>:9000.
     Unmapped controls go to /maschine/<name> as 0..1.
     map.json (next to this script) renames + rescales:
         {"knob/1": ["/rustjay/motion/red_gain", -2, 2]}
In:  OSC on :9001 -> LEDs. /maschine/led/<name> 0..1
     (names: pad/1..16, play, rec, group_a..h, softkey1..8, backlight, ...)
Local echo: buttons light while held, pads light with pressure.

Usage: python maschine-osc.py [target-ip] [--dump]
Needs: python-evdev, alsa-utils, user in 'audio' group.
"""
import json, os, socket, struct, subprocess, sys, threading
from evdev import InputDevice, ecodes, list_devices

CARD = "2"
OSC_OUT_PORT = 9000        # rustjay-engine OSC default
OSC_IN_PORT = 9001         # LED control in

# Button names from kernel sound/usb/caiaq/input.c (keycode_maschine),
# keyed by evdev code = BTN_MISC (0x100) + n.
BUTTONS = {0x100 + n: name for n, name in {
    0: "control", 1: "step", 2: "browse", 3: "sampling", 4: "left",
    5: "right", 6: "snap", 7: "autowrite", 8: "softkey8", 9: "softkey7",
    10: "softkey6", 11: "softkey5", 12: "softkey4", 13: "softkey3",
    14: "softkey2", 15: "softkey1", 16: "note_repeat", 17: "group_a",
    18: "group_b", 19: "group_c", 20: "group_d", 21: "group_e",
    22: "group_f", 23: "group_g", 24: "group_h", 25: "restart",
    26: "step_left", 27: "step_right", 28: "grid", 29: "play", 30: "rec",
    31: "erase", 32: "shift", 33: "scene", 34: "pattern", 35: "pad_mode",
    36: "navigate", 37: "duplicate", 38: "select", 39: "solo", 40: "mute",
}.items()}

# ABS axes: 8 knobs on HAT0X..HAT3Y (0..999), volume/tempo/swing encoders
# on RX/RY/RZ (0..999), 16 pressure pads on ABS_PRESSURE+i (0..4095).
AXES = {ecodes.ABS_HAT0X + i: (f"knob/{i + 1}", 999) for i in range(8)}
AXES[ecodes.ABS_RX] = ("volume", 999)
AXES[ecodes.ABS_RY] = ("tempo", 999)
AXES[ecodes.ABS_RZ] = ("swing", 999)
AXES.update({ecodes.ABS_PRESSURE + i: (f"pad/{i + 1}", 4095) for i in range(16)})

# ALSA HWDEP control numids on card 2 (amixer -c 2 controls), brightness 0..63.
LEDS = {f"pad/{i}": i for i in range(1, 17)}
LEDS.update({
    "mute": 17, "solo": 18, "select": 19, "duplicate": 20, "navigate": 21,
    "pad_mode": 22, "pattern": 23, "scene": 24, "shift": 25, "erase": 26,
    "grid": 27, "step_right": 28, "rec": 29, "play": 30, "step_left": 31,
    "restart": 32, "autowrite": 41, "snap": 42, "right": 43, "left": 44,
    "sampling": 45, "browse": 46, "step": 47, "control": 48,
    "note_repeat": 57, "backlight": 58,
})
LEDS.update({f"group_{c}": 33 + i for i, c in enumerate("abcdefgh")})
LEDS.update({f"softkey{i}": 48 + i for i in range(1, 9)})


def osc(addr, val):
    def s(b):
        return b + b"\0" * (4 - len(b) % 4)
    return s(addr.encode()) + s(b",f") + struct.pack(">f", float(val))


def osc_decode(data):
    # ponytail: single-float messages only — all this bridge ever receives
    addr = data.split(b"\0")[0].decode()
    return addr, struct.unpack(">f", data[-4:])[0]


assert osc("/a", 1.0) == b"/a\0\0,f\0\0" + struct.pack(">f", 1.0)
assert osc_decode(osc("/x", 0.5)) == ("/x", 0.5)


class Leds:
    def __init__(self):
        self.amixer = subprocess.Popen(
            ["amixer", "-c", CARD, "-s", "-q"],
            stdin=subprocess.PIPE, text=True)
        self.state = {}
        self.lock = threading.Lock()

    def set(self, name, val01):
        numid = LEDS.get(name)
        if numid is None:
            return
        v = max(0, min(63, round(val01 * 63)))
        with self.lock:
            if self.state.get(numid) == v:
                return
            self.state[numid] = v
            self.amixer.stdin.write(f"cset numid={numid} {v}\n")
            self.amixer.stdin.flush()

    def all_off(self):
        try:
            for numid in list(self.state):
                self.amixer.stdin.write(f"cset numid={numid} 0\n")
            self.amixer.stdin.flush()
        except Exception:
            pass


def led_listener(leds):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", OSC_IN_PORT))
    while True:
        data, _ = sock.recvfrom(512)
        try:
            addr, val = osc_decode(data)
        except Exception:
            continue
        if addr.startswith("/maschine/led/"):
            leds.set(addr[len("/maschine/led/"):], val)


def main():
    args = [a for a in sys.argv[1:] if a != "--dump"]
    dump = "--dump" in sys.argv
    target = args[0] if args else "192.168.1.137"
    mpath = os.path.join(os.path.dirname(os.path.abspath(__file__)), "map.json")
    mapping = json.load(open(mpath)) if os.path.exists(mpath) else {}
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    dev = next((d for d in map(InputDevice, list_devices())
                if "Maschine" in d.name), None)
    if dev is None:
        raise SystemExit("Maschine not found")
    leds = Leds()
    threading.Thread(target=led_listener, args=(leds,), daemon=True).start()

    leds.set("backlight", 1.0)  # hello: backlight on + pad flash
    for i in range(1, 17):
        leds.set(f"pad/{i}", 1.0)
    threading.Timer(0.4, lambda: [leds.set(f"pad/{i}", 0) for i in range(1, 17)]).start()

    print(f"reading '{dev.name}', OSC -> {target}:{OSC_OUT_PORT}, "
          f"LEDs <- :{OSC_IN_PORT}, {len(mapping)} mapped")
    try:
        for ev in dev.read_loop():
            if ev.type == ecodes.EV_KEY and ev.code in BUTTONS and ev.value != 2:
                name, val = "button/" + BUTTONS[ev.code], float(ev.value)
                leds.set(BUTTONS[ev.code], val)
            elif ev.type == ecodes.EV_ABS and ev.code in AXES:
                name, mx = AXES[ev.code]
                val = ev.value / mx
                if name.startswith("pad/"):
                    leds.set(name, val)
            else:
                continue
            m = mapping.get(name)
            addr = m[0] if m else "/maschine/" + name
            if m and len(m) == 3:
                val = m[1] + val * (m[2] - m[1])
            if dump:
                print(f"{name:20s} {val:8.3f} -> {addr}")
            sock.sendto(osc(addr, val), (target, OSC_OUT_PORT))
    finally:
        leds.all_off()


if __name__ == "__main__":
    main()
