#!/usr/bin/env python3
"""Records the board's serial log on macOS without resetting the chip.

`espflash monitor` (and so `just monitor-log`) resets the ESP32-S3 through
DTR/RTS when it connects. After a deep-sleep wake that reset replaces the
wake: the start then reports `ChipPowerOn` instead of `CoreDeepSleep`
(t-67). This reader opens the port read-only and leaves DTR/RTS as macOS
sets them on open: both asserted at once, which the USB-Serial-JTAG did
not treat as a reset on this board (observed on macOS, t-67; another OS
or driver may differ). It copies the bytes unchanged to stdout and to a
log, and reopens the port each time it comes back after a deep sleep. Only
the first ~0.15 s after a wake is lost, while the port re-enumerates. Stop
it with Ctrl-C.

Usage: passive-monitor.py LOG [PORT]   (PORT defaults to the first
/dev/cu.usbmodem* found, then stays that port). Standard library only.
"""

import glob
import os
import sys
import termios
import time
import tty


def find_port():
    """Returns the first USB modem port, or None."""
    ports = sorted(glob.glob("/dev/cu.usbmodem*"))
    return ports[0] if ports else None


def open_port(port):
    """Opens `port` read-only and raw, without hang-up on close."""
    fd = os.open(port, os.O_RDONLY | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        attrs = termios.tcgetattr(fd)
        tty.cfmakeraw(attrs)  # bytes unchanged: no CR/LF mapping, no XON/XOFF
        attrs[2] |= termios.CREAD | termios.CLOCAL
        attrs[2] &= ~termios.HUPCL  # keep DTR/RTS as they are on close
        termios.tcsetattr(fd, termios.TCSANOW, attrs)
    except BaseException:
        # A leaked fd still has HUPCL set: closing it at exit could reset.
        os.close(fd)
        raise
    return fd


def copy_until_gone(fd, port, sinks):
    """Copies data from `fd` to `sinks` until `port` disappears."""
    while True:
        try:
            data = os.read(fd, 4096)
        except BlockingIOError:
            data = b""
        except OSError:
            return
        if data:
            for sink in sinks:
                sink.write(data)
                sink.flush()
        elif not os.path.exists(port):
            return
        else:
            time.sleep(0.02)


def note(sinks, text):
    """Writes a `=== text` marker line to every sink."""
    line = f"\n=== {text} {time.strftime('%H:%M:%S')}\n".encode()
    for sink in sinks:
        sink.write(line)
        sink.flush()


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    log = sys.argv[1]
    port = sys.argv[2] if len(sys.argv) == 3 else None
    os.makedirs(os.path.dirname(log) or ".", exist_ok=True)
    with open(log, "wb") as out:
        sinks = [out, sys.stdout.buffer]
        last_error = None
        while True:
            # Locked to the first port found: with two boards plugged in, a
            # wake must not switch to the other one.
            port = port or find_port()
            if port is None or not os.path.exists(port):
                time.sleep(0.05)
                continue
            try:
                fd = open_port(port)
            except OSError as error:
                if str(error) != last_error:  # e.g. espflash holds the port
                    last_error = str(error)
                    note(sinks, f"cannot open {port}: {error}")
                time.sleep(0.05)
                continue
            last_error = None
            note(sinks, f"open {port}")
            try:
                copy_until_gone(fd, port, sinks)
            finally:
                os.close(fd)
            note(sinks, f"port gone {port}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
