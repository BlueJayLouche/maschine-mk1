#!/usr/bin/env python3
"""Bench shim: Maschine Mk1 Pi bridge <-> vp404, until the ESP32 firmware's
profile mapping replaces it.

Control in  :9000  /maschine/*            -> vp404 127.0.0.1:9002 /rustjay/*
Feedback in :9101  /rustjay/pads/pad<i>_loaded, /rustjay/transport/rec_state
                                          -> Pi :9001 /maschine/led/*

Sends /rustjay/sync 9101 every 5 s so vp404 (built with osc-feedback) adopts
us as the feedback target across restarts. Stdlib only, single-float OSC
messages — same wire format as bridge-python/maschine-osc.py.
"""

import socket
import struct
import sys
import threading

PI_LED = ("192.168.1.240", 9001)
VP404 = ("127.0.0.1", 9002)
CONTROL_PORT = 9000
FEEDBACK_PORT = 9101

# /maschine/<name> -> /rustjay/<addr>; pads are generated below.
MAP = {
    "knob/1": "/rustjay/pad/in_point",
    "knob/2": "/rustjay/pad/out_point",
    "button/rec": "/rustjay/transport/record",
    "button/erase": "/rustjay/transport/erase",
    "button/play": "/rustjay/transport/seq_play",
    "button/step": "/rustjay/transport/step_record",
    "button/note_repeat": "/rustjay/transport/retrigger",
    "button/step_right": "/rustjay/transport/pattern_next",
    "button/step_left": "/rustjay/transport/pattern_prev",
}
MAP.update({f"pad/{n}": f"/rustjay/pads/pad{n - 1}_trig" for n in range(1, 17)})

# feedback -> LED name on the Mk1
LED_MAP = {f"/rustjay/pads/pad{i}_loaded": f"pad/{i + 1}" for i in range(16)}
LED_MAP["/rustjay/transport/rec_state"] = "rec"

loaded = [0.0] * 16  # last known pad<i>_loaded, for post-release repaint


def pad4(b: bytes) -> bytes:
    return b + b"\0" * (4 - len(b) % 4)


def osc_f(addr: str, val: float) -> bytes:
    return pad4(addr.encode()) + pad4(b",f") + struct.pack(">f", val)


def osc_i(addr: str, val: int) -> bytes:
    return pad4(addr.encode()) + pad4(b",i") + struct.pack(">i", val)


def osc_decode(data: bytes):
    addr = data.split(b"\0")[0].decode()
    return addr, struct.unpack(">f", data[-4:])[0]


def main() -> None:
    ctrl = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    ctrl.bind(("0.0.0.0", CONTROL_PORT))
    fb = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    fb.bind(("0.0.0.0", FEEDBACK_PORT))
    out = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    warned: set[str] = set()

    def sync() -> None:
        out.sendto(osc_i("/rustjay/sync", FEEDBACK_PORT), VP404)
        threading.Timer(5.0, sync).start()

    def feedback_loop() -> None:
        while True:
            data, _ = fb.recvfrom(1536)
            try:
                addr, val = osc_decode(data)
            except (UnicodeDecodeError, struct.error):
                continue
            led = LED_MAP.get(addr)
            if led is None:
                continue  # sync dumps every param; we only relay LED sources
            if led.startswith("pad/"):
                loaded[int(led[4:]) - 1] = val
            out.sendto(osc_f(f"/maschine/led/{led}", val), PI_LED)
            print(f"led  {led:8s} <- {addr} = {val:.2f}")

    threading.Thread(target=feedback_loop, daemon=True).start()
    sync()
    print(f"shim up: :{CONTROL_PORT} -> {VP404}, feedback :{FEEDBACK_PORT} -> {PI_LED}")

    while True:
        data, _ = ctrl.recvfrom(1536)
        try:
            addr, val = osc_decode(data)
        except (UnicodeDecodeError, struct.error):
            continue
        name = addr.removeprefix("/maschine/")
        mapped = MAP.get(name)
        if mapped is None:
            if name not in warned:
                warned.add(name)
                print(f"unmapped: {addr}", file=sys.stderr)
            continue
        out.sendto(osc_f(mapped, val), VP404)
        # The Pi bridge's local pressure echo zeroes a pad LED on release —
        # repaint the loaded state shortly after.
        if name.startswith("pad/") and val <= 0.05:
            n = int(name[4:])
            if loaded[n - 1] > 0.0:
                threading.Timer(
                    0.15,
                    lambda n=n: out.sendto(
                        osc_f(f"/maschine/led/pad/{n}", loaded[n - 1]), PI_LED
                    ),
                ).start()


if __name__ == "__main__":
    main()
