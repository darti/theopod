#!/usr/bin/env python3
"""Bounds the stack depth of the firmware's tasks from the disassembly.

Frame sizes are only visible in the disassembly: the xtensa `entry a1, N`
immediate or, past 32 KiB, an `l32r` literal subtracted from a1
(`sub aX, a1, aY; movsp a1, aX`). From each task's poll function this walks
the direct calls (`callN`) and the indirect calls whose target an `l32r` loads
(`callxN`), and sums the deepest chain of frames. Calls through vtables are not
resolved: keep a margin below the real stack size. A 53 KB request frame on
core1's 48 KiB SD owner stack, then a 99 KB network chain on core0's 47 KB main
stack, both overflowed on the board (t-133).

Usage: frame-check.py DISASSEMBLY PATTERN=LIMIT [PATTERN=LIMIT ...]
DISASSEMBLY is `xtensa-esp32s3-elf-objdump -d -C` output of the firmware.
Each embassy task poll (`TaskStorage<...>::poll`) is bounded by the first
PATTERN its name contains. Exits 1 when a task's deepest chain exceeds its
LIMIT bytes, or when a PATTERN matches no task. Standard library only.
"""

import re
import sys

HEADER = re.compile(r"^([0-9a-f]+) <(.*)>:$")
ENTRY = re.compile(r"\bentry\s+a1, (0x[0-9a-f]+|\d+)")
# objdump appends `<symbol>` only when a symbol happens to be near the value.
LITERAL = re.compile(r"\bl32r\s+(a\d+), .*\(([0-9a-f]+)(?: <[^()]*>)?\)\s*$")
SUB_SP = re.compile(r"\bsub\s+(a\d+), a1, (a\d+)")
MOVSP = re.compile(r"\bmovsp\s+a1, (a\d+)")
CALL = re.compile(r"\bcall(?:0|4|8|12)\s+([0-9a-f]+)\b")
CALLX = re.compile(r"\bcallx(?:0|4|8|12)\s+(a\d+)")
TASK = "TaskStorage<"
PROLOGUE_LINES = 24


def parse(lines):
    """Returns ({function: frame bytes}, {function: callees})."""
    functions, current = {}, None
    for line in lines:
        header = HEADER.match(line)
        if header:
            current = header.group(2)
            functions[current] = (int(header.group(1), 16), [])
        elif current is not None:
            functions[current][1].append(line)
    names = {address: name for name, (address, _) in functions.items()}
    frames, calls = {}, {}
    for name, (_, body) in functions.items():
        frames[name] = frame(body)
        calls[name] = callees(body, names)
    return frames, calls


def frame(body):
    """Frame bytes from a function prologue."""
    size, literals, moved = 0, {}, {}
    for text in body[:PROLOGUE_LINES]:
        entry = ENTRY.search(text)
        if entry and not size:
            size = int(entry.group(1), 0)
        literal = LITERAL.search(text)
        if literal:
            literals[literal.group(1)] = int(literal.group(2), 16)
        sub = SUB_SP.search(text)
        if sub and sub.group(2) in literals:
            moved[sub.group(1)] = literals[sub.group(2)]
        movsp = MOVSP.search(text)
        if movsp:
            # The frame is the literal subtracted from a1 into this register.
            size = max(size, moved.get(movsp.group(1), 0))
            break
    return size


def callees(body, names):
    """Functions called directly, or through a literal loaded address."""
    found, literals = set(), {}
    for text in body:
        literal = LITERAL.search(text)
        if literal:
            literals[literal.group(1)] = int(literal.group(2), 16)
            continue
        call = CALL.search(text)
        if call and int(call.group(1), 16) in names:
            found.add(names[int(call.group(1), 16)])
        callx = CALLX.search(text)
        if callx and literals.get(callx.group(1)) in names:
            found.add(names[literals[callx.group(1)]])
    return found


def deepest(root, frames, calls):
    """(bytes, chain) of the deepest call chain from root; cycles count once."""
    memo, active = {}, set()

    def walk(name):
        if name in memo:
            return memo[name]
        if name in active:
            return 0, []
        active.add(name)
        best = (0, [])
        for callee in calls.get(name, ()):
            below = walk(callee)
            if below[0] > best[0]:
                best = below
        active.discard(name)
        memo[name] = (frames.get(name, 0) + best[0], [name] + best[1])
        return memo[name]

    sys.setrecursionlimit(max(sys.getrecursionlimit(), 4 * len(frames) + 1000))
    return walk(root)


def main():
    if len(sys.argv) < 3 or any("=" not in arg for arg in sys.argv[2:]):
        sys.exit(__doc__)
    bounds = []
    for arg in sys.argv[2:]:
        pattern, limit = arg.rsplit("=", 1)
        bounds.append((pattern, int(limit)))
    with open(sys.argv[1], errors="replace") as dump:
        frames, calls = parse(dump)
    tasks = [name for name in frames if TASK in name and name.endswith(">::poll")]
    failed, used = [], set()
    for task in sorted(tasks):
        bound = next(((p, limit) for p, limit in bounds if p in task), None)
        if bound is None:
            continue
        used.add(bound[0])
        size, chain = deepest(task, frames, calls)
        over = size > bound[1]
        print(f"{'OVER' if over else 'ok  '} {size:6} / {bound[1]:6} {task[:140]}")
        if over:
            failed.append(task)
            for name in chain[:16]:
                print(f"         {frames.get(name, 0):6} {name[:140]}")
    missing = [pattern for pattern, _ in bounds if pattern not in used]
    if missing:
        sys.exit(f"frame-check: no task matches {', '.join(missing)}")
    if failed:
        sys.exit(f"frame-check: {len(failed)} task stack(s) over their bound")


if __name__ == "__main__":
    main()
