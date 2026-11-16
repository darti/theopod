#!/usr/bin/env python3
"""Group opencore-aacdec fuzz artifacts by crash site (t-169).

Usage: triage.py FUZZER ARTIFACT_DIR

Replays every artifact with the libFuzzer binary, keeps the sanitizer's
SUMMARY line, and prints one line per distinct site with the smallest
artifact that reaches it (the one to minimize and keep as a regression
input). Set ASAN_SYMBOLIZER_PATH to llvm-symbolizer: the macOS default
(atos) takes minutes per report. Standard library only (tools/ convention).
"""

import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

SUMMARY = re.compile(r"^SUMMARY: (\w+): (.*)$", re.M)


REPLAY_TIMEOUT = 120


def site(fuzzer, artifact):
    try:
        run = subprocess.run(
            [fuzzer, "-timeout=10", str(artifact)],
            capture_output=True,
            text=True,
            errors="replace",
            timeout=REPLAY_TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        return f"replay over {REPLAY_TIMEOUT} s"
    found = [m for m in SUMMARY.finditer(run.stderr) if m.group(1) != "libFuzzer"]
    if found:
        tool, text = found[0].groups()
        # Drop absolute paths and columns: one bucket per file:line.
        text = re.sub(r"\S*/([^/\s]+:\d+)(:\d+)?", r"\1", text)
        return f"{tool}: {text}"
    if run.returncode == 0:
        return "no crash (fixed?)"
    return f"exit {run.returncode}: {run.stderr.strip().splitlines()[-1:]}"


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    fuzzer, directory = sys.argv[1], Path(sys.argv[2])
    artifacts = sorted(p for p in directory.iterdir() if p.is_file())
    with ThreadPoolExecutor() as pool:
        sites = list(pool.map(lambda a: site(fuzzer, a), artifacts))
    buckets = {}
    for artifact, where in zip(artifacts, sites):
        buckets.setdefault(where, []).append(artifact)
    for where, members in sorted(buckets.items(), key=lambda kv: -len(kv[1])):
        smallest = min(members, key=lambda p: p.stat().st_size)
        print(f"{len(members):5}  {where}\n       smallest: {smallest} ({smallest.stat().st_size} B)")


if __name__ == "__main__":
    main()
