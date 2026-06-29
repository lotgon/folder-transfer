#!/usr/bin/env python3
"""Build the same corpora bench.ps1 uses, under <root>. Prints the deflate ratio
of the compressible block (raw deflate, level 1 ~ .NET Fastest)."""
import os
import sys
import random
import zlib

root = sys.argv[1]
TINY_COUNT = 10000
TINY_KB = 4
LARGE_MB = 4
MB = 1024 * 1024

def ensure(d):
    os.makedirs(d, exist_ok=True)

# tiny: 10000 x 4KB (one shared random block, like bench.ps1)
tiny = os.path.join(root, "tiny", "F")
ensure(tiny)
tb = os.urandom(TINY_KB * 1024)
for i in range(TINY_COUNT):
    with open(os.path.join(tiny, f"f{i}.bin"), "wb") as f:
        f.write(tb)

# compressible base block ~LARGE_MB of natural-ish text (same word list as bench.ps1)
words = ("the quick brown fox jumps over a lazy dog lorem ipsum dolor sit amet consectetur adipiscing "
         "elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis").split(" ")
rnd = random.Random(12345)
parts = []
size = 0
target = LARGE_MB * MB
while size < target:
    w = words[rnd.randrange(len(words))]
    parts.append(w)
    parts.append(" ")
    size += len(w) + 1
    if rnd.randrange(12) == 0:
        parts.append("\r\n")
        size += 2
base = "".join(parts)
bb = base.encode("utf-8")
co = zlib.compressobj(1, zlib.DEFLATED, -15)  # raw deflate
comp = co.compress(bb) + co.flush()
ratio = len(bb) / len(comp)

def build_comp(name, mb):
    d = os.path.join(root, name, "F")
    ensure(d)
    n = max(1, mb // LARGE_MB)
    for i in range(1, n + 1):
        with open(os.path.join(d, f"doc{i}.txt"), "w", encoding="utf-8", newline="") as f:
            f.write(f"file {i}\r\n")
            f.write(base)

def build_inc(name, mb):
    d = os.path.join(root, name, "F")
    ensure(d)
    n = max(1, mb // LARGE_MB)
    rb = os.urandom(LARGE_MB * MB)  # one shared random block (incompressible per file)
    for i in range(1, n + 1):
        with open(os.path.join(d, f"rnd{i}.bin"), "wb") as f:
            f.write(rb)

build_comp("cmp_s", 30); build_comp("cmp_m", 150); build_comp("cmp_l", 300)
build_inc("inc_s", 30); build_inc("inc_m", 150); build_inc("inc_l", 300)

print(f"RATIO={ratio:.2f}")
