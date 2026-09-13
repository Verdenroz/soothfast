#!/usr/bin/env python3
"""bind bench harness: soothfast_stats vs. the same arithmetic in Python.

One deterministic LCG feeds every language the same 100k doubles, so a
ratio here means the same thing it means anywhere else `bind bench` runs.
"""

import array
import json
import sys
import time

import soothfast_stats

N = 100_000
K = 9

LCG_A = 6364136223846793005
LCG_C = 1442695040888963407
LCG_MASK = (1 << 64) - 1


def samples(n):
    x = 7
    out = []
    for _ in range(n):
        x = (x * LCG_A + LCG_C) & LCG_MASK
        out.append((x >> 11) / float(1 << 53))
    return out


def median_ns(fn):
    times = []
    last = None
    for _ in range(K):
        t0 = time.perf_counter_ns()
        last = fn()
        t1 = time.perf_counter_ns()
        times.append(t1 - t0)
    times.sort()
    return times[K // 2], last


def host_median_mad(values):
    ordered = sorted(values)
    n = len(ordered)
    if n % 2:
        median = ordered[n // 2]
    else:
        median = (ordered[n // 2 - 1] + ordered[n // 2]) / 2.0
    abs_devs = sorted(abs(v - median) for v in values)
    if n % 2:
        mad = abs_devs[n // 2]
    else:
        mad = (abs_devs[n // 2 - 1] + abs_devs[n // 2]) / 2.0
    return median, mad


def dev(value, median, mad):
    if mad == 0.0:
        return 0.0 if value == median else float("inf")
    return abs(value - median) / mad


def emit(shape, binding_ns, host_ns, n):
    print(json.dumps({"shape": shape, "binding_ns": binding_ns, "host_ns": host_ns, "n": n}))


values = samples(N)
buf = array.array("d", values)
checksum = 0.0

binding_ns, s = median_ns(lambda: soothfast_stats.Summary(buf))
host_ns, (median, mad) = median_ns(lambda: host_median_mad(values))
checksum += s.median
emit("build_summary", binding_ns, host_ns, N)

summary = soothfast_stats.Summary(buf)

binding_ns, r = median_ns(lambda: summary.deviations_all(buf))
host_ns, r2 = median_ns(lambda: [dev(v, median, mad) for v in values])
checksum += r[0] + r2[0]
emit("batch_buffer", binding_ns, host_ns, N)

out_buf = array.array("d", [0.0] * N)
host_out = [0.0] * N


def host_into():
    for i, v in enumerate(values):
        host_out[i] = dev(v, median, mad)
    return host_out


binding_ns, _ = median_ns(lambda: summary.deviations_into(buf, out_buf))
host_ns, _ = median_ns(host_into)
checksum += out_buf[0] + host_out[0]
emit("batch_into", binding_ns, host_ns, N)


def binding_per_element():
    total = 0.0
    for v in values:
        total += summary.deviations(v)
    return total


def host_per_element():
    total = 0.0
    for v in values:
        total += dev(v, median, mad)
    return total


binding_ns, t1 = median_ns(binding_per_element)
host_ns, t2 = median_ns(host_per_element)
checksum += t1 + t2
emit("per_element", binding_ns, host_ns, N)

print(f"checksum: {checksum}", file=sys.stderr)
