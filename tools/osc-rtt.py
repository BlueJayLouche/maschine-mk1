#!/usr/bin/env python3
"""Measure controller OSC round-trip time.

Sends /maschine/ping to the ESP32's :9001 and times the /maschine/pong echo
(answered in the firmware's socket thread — the number is WiFi + UDP stack,
no engine hop). Stdlib only.

Usage: osc-rtt.py <esp32-ip> [count]
"""

import socket
import statistics
import struct
import sys
import time


def pad4(b: bytes) -> bytes:
    return b + b"\0" * (4 - len(b) % 4)


def osc_f(addr: str, val: float) -> bytes:
    return pad4(addr.encode()) + pad4(b",f") + struct.pack(">f", val)


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__.strip())
    ip = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 100

    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(1.0)
    rtts, lost = [], 0
    for i in range(count):
        t0 = time.perf_counter()
        s.sendto(osc_f("/maschine/ping", float(i)), (ip, 9001))
        try:
            while True:
                data, _ = s.recvfrom(512)
                if struct.unpack(">f", data[-4:])[0] == float(i):
                    rtts.append((time.perf_counter() - t0) * 1000)
                    break
        except socket.timeout:
            lost += 1
        time.sleep(0.02)

    if not rtts:
        sys.exit("no replies — is the firmware on WiFi and :9001 up?")
    r = sorted(rtts)
    p95 = r[min(len(r) - 1, int(len(r) * 0.95))]
    print(
        f"{len(r)}/{count} replies  min {r[0]:.1f}  "
        f"median {statistics.median(r):.1f}  p95 {p95:.1f}  max {r[-1]:.1f} ms"
        + (f"  lost {lost}" if lost else "")
    )


if __name__ == "__main__":
    main()
