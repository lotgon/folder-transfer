# Benchmark methodology

This explains how [BENCHMARKS.md](../BENCHMARKS.md) is produced and how to reproduce it yourself.
All of it is two self-contained PowerShell scripts:

- [bench/bench.ps1](bench.ps1) - the orchestrator that builds corpora, runs every transfer, and writes `BENCHMARKS.md`.
- [bench/bench-proxy.ps1](bench-proxy.ps1) - a local TCP proxy that emulates a WAN link (bandwidth cap + latency).

The code under test is the real tool: [ft-server.ps1](../ft-server.ps1) and [ft-client.ps1](../ft-client.ps1).

## Reproduce it

From the repo root, on an **idle** machine:

```
powershell -NoProfile -ExecutionPolicy Bypass -File bench\bench.ps1
```

It regenerates `BENCHMARKS.md` at the repo root in ~5-8 minutes. No admin, no install, nothing
to clean up - it works in a scratch dir and removes it afterwards.

Useful switches:

| switch | meaning |
|---|---|
| `-WorkDir <path>` | scratch root for corpora and destination (default `<SystemDrive>\ft-bench`). |
| `-TinyCount <n>` / `-TinySizeKB <n>` | size of the small-files corpus (default 10000 x 4 KB). |
| `-LargeFileMB <n>` | size of each large file (default 4 MB; > 1 MB so it uses the compression path). |
| `-MbSlow` / `-MbMid` / `-MbFast <n>` | large-corpus size in MB per bandwidth column (default 30 / 150 / 300 for 20 / 100 / 200 Mbit; the fast size is also the LAN reference). |
| `-KeepTmp` | keep the scratch dir and per-run logs (`<WorkDir>\<pid>\logs`) for debugging. |

## What the table shows

A grid of **efficiency**, one row per data type, one column per (channel, ping):

```
efficiency = goodput / channel capacity
```

`goodput` is original bytes per second actually synced (what the receiver gains), reported by the
client. `channel capacity` is the raw wire rate of the emulated link. So:

- **~100%** - the link is saturated; we are moving data as fast as the wire allows.
- **above 100%** - adaptive compression delivered more original data than the wire physically
  carried (it sends fewer bytes). Only possible on compressible data.
- **below 100%** - the bottleneck is off-link (for small files: file creation on the receiver).

Efficiency is largely transferable between machines; the absolute capacities and the file-creation
floor printed under the table are machine-specific.

### Rows - data types

| row | corpus | exercises |
|---|---|---|
| small files | many tiny files (default 10000 x 4 KB, random) | per-file overhead + receiver file-creation cost; sent in bundles (never compressed). |
| large, incompressible | random bytes, 4 MB files | the large-file path with nothing to compress - adaptive must stay raw. |
| large, compressible | natural-ish text, 4 MB files | the large-file path where compression pays - adaptive must turn it on. |

The large corpora are **sized per channel** (30 / 150 / 300 MB for 20 / 100 / 200 Mbit) so every run
lasts long enough that fixed per-run overhead (handshakes, producer spin-up) does not skew the fast
columns. The compressible corpus prints its real deflate ratio so the >100% numbers are interpretable.

### Columns - channels and ping

Each bandwidth is tested at 0 ms and at 150 ms round-trip:

| column | wire rate | RTT |
|---|---|---|
| 20 Mbit | 2.5 MB/s | 0 ms |
| 20 Mbit +150ms | 2.5 MB/s | 150 ms |
| 100 Mbit | 12.5 MB/s | 0 ms |
| 100 Mbit +150ms | 12.5 MB/s | 150 ms |
| 200 Mbit | 25 MB/s | 0 ms |
| 200 Mbit +150ms | 25 MB/s | 150 ms |

LAN (loopback) is **not** an efficiency column: with no bandwidth limit the link is never the
bottleneck, so "% of link used" is not meaningful (and a single loopback measurement is noisy).
LAN is still run, but reported as raw goodput (MB/s) on a reference line beneath the table.

## How a WAN link is emulated

[bench-proxy.ps1](bench-proxy.ps1) sits between client and server and forwards bytes while:

- **adding latency** - each chunk is released only after a fixed one-way delay (RTT = 2x delay),
  decoupled from bandwidth via a delay queue so latency alone does not throttle throughput.
- **capping bandwidth** - a single, GLOBAL rate gate shared by all connections, because a real WAN
  link is one pipe shared by every parallel stream (4 streams must not get 4x the bandwidth).
- **propagating backpressure** - when throttling, the in-flight buffer is bounded to a small,
  realistic link buffer so the sender actually feels the slow link (an unbounded proxy buffer would
  drain the sender instantly and hide the real rate).
- **half-closing correctly** - on EOF it shuts down only the destination's send side, so the
  reverse direction keeps flowing (the classic client half-closes after its request).

Latency tests use no rate cap; bandwidth tests use no extra buffering beyond the small link buffer.

## How a single cell is measured

For every cell, `Run-Xfer` in [bench.ps1](bench.ps1):

1. kills any leftover server/proxy and clears the destination,
2. (for throttled cells) starts the proxy and waits until it listens,
3. starts a fresh `ft-server.ps1` on a fixed port and waits for the generated client `.bat`,
4. extracts the token + TLS fingerprint from that `.bat`,
5. runs `ft-client.ps1` against the server (or the proxy) and waits for it to finish,
6. parses the goodput from the client's `sync done ... @ X MB/s` line.

The client computes MB/s from a sub-second timer, so the figure is accurate even when a transfer
finishes in well under a second. The "bare file-creation floor" under the small-files note is a
native (C#) probe that just creates the same number of files locally, to show how much of the
small-files result is the disk and not the protocol.

## Caveats

- **Run on an idle machine.** The benchmark spawns many short-lived processes; background load (or
  a still-flushing disk) depresses especially the small-files numbers.
- **Avoid AppData / %TEMP% as the work dir.** On many Windows setups it sits behind a filter driver
  (indexer / AV) that makes small-file creation several times slower. The default work dir is a
  clean root-level folder for this reason; override with `-WorkDir`.
- **Antivirus matters.** Real-time scanning of newly created files is a real cost for the
  small-files row; the test machine's Defender state is printed in the report.
