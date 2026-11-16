#!/usr/bin/env python3
"""Write opencore-aacdec fuzz seeds from the bench AAC clip (t-169).

Usage: seeds.py CLIP_DIR CORPUS_DIR

CLIP_DIR holds `aac.bin`, `aac.len` and `aac.asc` as written by
`tools/audio-bench/clip.py` (`just audio-bench-inputs`): copyrighted audio,
so the seeds stay under target/ too. Each seed follows the driver's layout
(tools/aacdec-fuzz/harness/driver.cpp):

- raw: the clip's AudioSpecificConfig (AAC-LC), and variants signalling
  SBR (AOT 5) and parametric stereo (AOT 29) explicitly, or SBR backward
  compatibly (sync extension 0x2b7), each followed by 8 access units;
- ADTS: the same access units behind ADTS headers, without and with CRC.

Standard library only (tools/ convention).
"""

import struct
import sys
from pathlib import Path

FRAMES = 8


def bits_to_bytes(bits):
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i : i + 8], 2) for i in range(0, len(bits), 8))


def asc_variants(asc):
    """The clip's ASC and SBR/PS signalling variants on the same core."""
    head = int.from_bytes(asc[:2], "big")
    aot, sfi, chans = head >> 11, (head >> 7) & 0xF, (head >> 3) & 0xF
    if aot != 2:
        raise SystemExit(f"expected an AAC-LC clip, got AOT {aot}")
    ga = "000"  # frameLengthFlag, dependsOnCoreCoder, extensionFlag
    core = f"{sfi:04b}{chans:04b}"
    # Explicit SBR/PS: the extension rate doubles the core rate (index - 3).
    ext = f"{max(sfi - 3, 0):04b}"
    implicit = f"{2:05b}{core}{ga}" + f"{0x2B7:011b}{5:05b}1{ext}"
    return {
        "lc": asc,
        "sbr": bits_to_bytes(f"{5:05b}{core}{ext}{2:05b}{ga}"),
        "ps": bits_to_bytes(f"{29:05b}{core}{ext}{2:05b}{ga}"),
        "sbr-implicit": bits_to_bytes(implicit),
    }


def adts_header(frame, sfi, chans, crc):
    """An MPEG-4 AAC-LC ADTS header (one raw data block) for `frame`."""
    length = 7 + (2 if crc else 0) + len(frame)
    bits = (
        f"{0xFFF:012b}0{0:02b}{0 if crc else 1:01b}"
        f"{1:02b}{sfi:04b}0{chans:03b}00"
        f"00{length:013b}{0x7FF:011b}{0:02b}"
    )
    return bits_to_bytes(bits) + (b"\x00\x00" if crc else b"")


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    clip, corpus = Path(sys.argv[1]), Path(sys.argv[2])
    data = (clip / "aac.bin").read_bytes()
    sizes = (clip / "aac.len").read_bytes()
    asc = (clip / "aac.asc").read_bytes()
    lengths = [v for (v,) in struct.iter_unpack("<H", sizes)]
    offsets = [sum(lengths[:i]) for i in range(len(lengths))]
    head = int.from_bytes(asc[:2], "big")
    sfi, chans = (head >> 7) & 0xF, (head >> 3) & 0xF
    corpus.mkdir(parents=True, exist_ok=True)
    count = 0
    # A few windows across the clip: silence at the start, speech further on.
    for window in range(0, len(lengths) - FRAMES, max(len(lengths) // 4, 1)):
        frames = [data[offsets[i] : offsets[i] + lengths[i]] for i in range(window, window + FRAMES)]
        for name, config in asc_variants(asc).items():
            body = b"".join(struct.pack("<H", len(f)) + f for f in frames)
            (corpus / f"raw-{name}-{window}").write_bytes(bytes([0, len(config)]) + config + body)
            count += 1
        for crc in (False, True):
            stream = b"".join(adts_header(f, sfi, chans, crc) + f for f in frames)
            (corpus / f"adts-{'crc' if crc else 'nocrc'}-{window}").write_bytes(b"\x01" + stream)
            count += 1
    print(f"seeds: {count} written to {corpus}")


if __name__ == "__main__":
    main()
