"""Write test vectors for the parity RNG (`crates/data-fill/src/parity_rng.rs`) from CPython's
own `random.Random`, so the Rust port is checked against the reference, not against itself.

Usage: python3 -I parity_rng_vectors.py > crates/data-fill/tests/parity_rng_vectors.json
Integers above 2^53 and float bit patterns are written as decimal strings.
"""

from __future__ import annotations

import json
import random
import struct
import sys


def big(n: int) -> str:
    return str(n)


def fbits(x: float) -> str:
    return str(struct.unpack("<Q", struct.pack("<d", x))[0])


SEEDS = [0, 1, 2, 7, 42, 12345, 2**32 - 1, 2**32, 2**63 + 11, 2**64 - 1, 2**64 + 5, 3**100, -7]


def case(seed: int) -> dict:
    out: dict = {"seed": big(seed)}
    r = random.Random(seed)
    out["u32"] = [r.getrandbits(32) for _ in range(8)]
    r = random.Random(seed)
    out["bits"] = [[k, big(r.getrandbits(k))] for k in (0, 1, 5, 31, 32, 33, 53, 64, 65, 100, 128, 7)]
    r = random.Random(seed)
    out["random"] = [fbits(r.random()) for _ in range(6)]
    r = random.Random(seed)
    out["randrange"] = [[a, b, r.randrange(a, b)] for a, b in ((0, 1), (0, 2), (0, 3), (5, 6), (-10, 10),
                                                                 (0, 201), (0, 1000), (0, 2**40))]
    r = random.Random(seed)
    out["randint"] = [[a, b, r.randint(a, b)] for a, b in ((0, 200), (0, 0), (1, 6), (-3, 3), (0, 65535))]
    r = random.Random(seed)
    out["choice"] = [[n, r.choice(range(n))] for n in (1, 1, 2, 6, 37, 1000)]
    out["shuffle"] = []
    for n in (0, 1, 2, 5, 30, 200):
        r = random.Random(seed)
        xs = list(range(n))
        r.shuffle(xs)
        out["shuffle"].append(xs)
    out["sample"] = []
    for n, k in ((30, 0), (30, 1), (30, 5), (30, 6), (30, 29), (30, 30), (21, 3), (22, 3), (100, 3),
                 (2000, 10), (2000, 100), (5, 5)):
        r = random.Random(seed)
        out["sample"].append([n, k, r.sample(range(n), k)])
    out["choices"] = []
    r = random.Random(seed)
    out["choices"].append([[], 5, r.choices(range(7), k=5)])
    r = random.Random(seed)
    out["choices"].append([[1, 2, 3, 0, 4], 6, r.choices(range(5), weights=[1, 2, 3, 0, 4], k=6)])
    r = random.Random(seed)
    out["choices"].append([[50, 1], 4, r.choices(range(2), weights=[50, 1], k=4)])
    # Mixed stream: the exact sequence of different calls on one generator.
    r = random.Random(seed)
    mixed = []
    for _ in range(3):
        mixed.append(r.randint(0, 200))
        mixed.append(r.getrandbits(32))
        mixed.append(r.choice(range(1)))
        xs = list(range(9))
        r.shuffle(xs)
        mixed.append(xs)
    out["mixed"] = mixed
    # Derived generator: a child seeded from 64 bits of the parent.
    r = random.Random(seed)
    child = random.Random(r.getrandbits(64))
    out["child_u32"] = [child.getrandbits(32) for _ in range(4)]
    return out


def main() -> None:
    json.dump({"python": sys.version.split()[0], "cases": [case(s) for s in SEEDS]}, sys.stdout,
              separators=(",", ":"))
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
