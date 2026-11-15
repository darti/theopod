#!/usr/bin/env python3
"""Cut a decoder benchmark clip from a podcast enclosure (t-164).

Usage: audio-clip.py aac|mp3 URL OUT_DIR [--skip SECONDS] [--seconds SECONDS]

aac: reads the faststart MP4 index (moov) with Range requests, then writes
     `aac.bin` (raw AAC access units, back to back), `aac.len` (u16 LE size per
     frame) and `aac.asc` (AudioSpecificConfig from esds).
mp3: skips the ID3v2 tag and writes `mp3.bin` (whole MPEG frames from the
     first sync at or after --skip seconds of 128 kbps CBR).

Standard library only (tools/ convention). The clips are copyrighted audio:
they stay under target/ and are never committed.
"""

import http.client
import struct
import sys
import urllib.request


def fetch(url, start, end, piece=128 * 1024, attempts=5):
    """Read bytes start..=end in Range pieces, retrying truncated reads."""
    parts = []
    for first in range(start, end + 1, piece):
        last = min(first + piece - 1, end)
        request = urllib.request.Request(url, headers={"Range": f"bytes={first}-{last}"})
        for attempt in range(attempts):
            try:
                with urllib.request.urlopen(request, timeout=60) as response:
                    if response.status != 206:
                        raise SystemExit(f"expected 206, got {response.status}")
                    parts.append(response.read())
                    break
            except http.client.IncompleteRead:
                if attempt + 1 == attempts:
                    raise
    data = b"".join(parts)
    if len(data) != end + 1 - start:
        raise SystemExit("short read")
    return data


def boxes(data, offset, end):
    while offset + 8 <= end:
        size, kind = struct.unpack(">I4s", data[offset : offset + 8])
        header = 8
        if size == 1:
            size = struct.unpack(">Q", data[offset + 8 : offset + 16])[0]
            header = 16
        if size < header:
            raise SystemExit("bad box size")
        yield kind.decode("latin1"), offset + header, offset + size
        offset += size


def child(data, start, end, path):
    for name in path:
        for kind, body, stop in boxes(data, start, end):
            if kind == name:
                start, end = body, stop
                break
        else:
            raise SystemExit(f"missing box {name}")
    return start, end


def descriptor(data, at):
    tag = data[at]
    at += 1
    size = 0
    while True:
        byte = data[at]
        at += 1
        size = size << 7 | byte & 0x7F
        if not byte & 0x80:
            return tag, at, size


def audio_specific_config(esds):
    at = 4  # version + flags
    tag, at, _ = descriptor(esds, at)
    assert tag == 3
    flags = esds[at + 2]
    at += 3 + (2 if flags & 0x80 else 0) + (2 if flags & 0x20 else 0)
    if flags & 0x40:
        at += 1 + esds[at]
    tag, at, _ = descriptor(esds, at)
    assert tag == 4
    at += 13
    tag, at, size = descriptor(esds, at)
    assert tag == 5
    return esds[at : at + size]


def aac(url, out, skip, seconds):
    head = fetch(url, 0, 65535)
    moov = None
    for kind, body, stop in boxes(head, 0, len(head)):
        if kind == "moov":
            moov = (body - 8, stop)
            break
    if moov is None:
        raise SystemExit("moov is not first: not a faststart file")
    data = fetch(url, 0, moov[1] - 1)
    stbl = child(data, moov[0] + 8, moov[1], ["trak", "mdia", "minf", "stbl"])
    tables = {kind: (body, stop) for kind, body, stop in boxes(data, *stbl)}
    stsd = tables["stsd"][0] + 8
    esds = data.find(b"esds", stsd)
    esds_size = struct.unpack(">I", data[esds - 4 : esds])[0]
    asc = audio_specific_config(data[esds + 4 : esds - 4 + esds_size])
    rate = struct.unpack(">I", data[stsd + 8 + 24 : stsd + 8 + 28])[0] >> 16

    body = tables["stsz"][0]
    constant, count = struct.unpack(">II", data[body + 4 : body + 12])
    assert constant == 0
    sizes = struct.unpack(f">{count}I", data[body + 12 : body + 12 + 4 * count])
    body = tables["stco"][0]
    chunks = struct.unpack(">I", data[body + 4 : body + 8])[0]
    offsets = struct.unpack(f">{chunks}I", data[body + 8 : body + 8 + 4 * chunks])
    body = tables["stsc"][0]
    entries = struct.unpack(">I", data[body + 4 : body + 8])[0]
    runs = [
        struct.unpack(">III", data[body + 8 + 12 * i : body + 20 + 12 * i])
        for i in range(entries)
    ]

    positions = []
    frame = 0
    for index, offset in enumerate(offsets, start=1):
        per_chunk = [run[1] for run in runs if run[0] <= index][-1]
        for _ in range(per_chunk):
            positions.append(offset)
            offset += sizes[frame]
            frame += 1
    first = int(skip * rate / 1024)
    last = first + int(seconds * rate / 1024)
    start = positions[first]
    end = positions[last - 1] + sizes[last - 1]
    if any(positions[i] + sizes[i] != positions[i + 1] for i in range(first, last - 1)):
        raise SystemExit("clip spans non-contiguous chunks")
    payload = fetch(url, start, end - 1)
    open(f"{out}/aac.bin", "wb").write(payload)
    open(f"{out}/aac.len", "wb").write(struct.pack(f"<{last - first}H", *sizes[first:last]))
    open(f"{out}/aac.asc", "wb").write(asc)
    print(f"aac: {last - first} frames, {len(payload)} bytes, {rate} Hz, asc {asc.hex()}")


def mp3(url, out, skip, seconds):
    head = fetch(url, 0, 9)
    tag = 0
    if head[:3] == b"ID3":
        tag = 10 + (head[6] << 21 | head[7] << 14 | head[8] << 7 | head[9])
    byte_rate = 128_000 // 8
    start = tag + int(skip * byte_rate)
    data = fetch(url, start, start + int(seconds * byte_rate) + 4096 - 1)
    at = 0
    while not (data[at] == 0xFF and data[at + 1] & 0xE0 == 0xE0 and data[at + 2] >> 4 == 9):
        at += 1
    data = data[at : at + int(seconds * byte_rate)]
    open(f"{out}/mp3.bin", "wb").write(data)
    print(f"mp3: {len(data)} bytes from offset {start + at}")


def main():
    args = sys.argv[1:]
    options = {"--skip": 120.0, "--seconds": 30.0}
    for name in list(options):
        if name in args:
            at = args.index(name)
            options[name] = float(args[at + 1])
            del args[at : at + 2]
    if len(args) != 3 or args[0] not in ("aac", "mp3"):
        raise SystemExit(__doc__)
    kind, url, out = args
    (aac if kind == "aac" else mp3)(url, out, options["--skip"], options["--seconds"])


if __name__ == "__main__":
    main()
