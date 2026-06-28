# folder-transfer benchmarks

Generated: 2026-06-28 18:09  |  Version: v0.13.0

> Efficiency (%) is largely transferable across machines; the underlying capacities and the
> file-creation floor below are SPECIFIC TO THIS MACHINE (CPU, disk, antivirus). Regenerate
> with `powershell -NoProfile -ExecutionPolicy Bypass -File bench\bench.ps1`.

## Test machine

| | |
|---|---|
| OS | Microsoft Windows 11 Enterprise (10.0.26200) |
| CPU | 13th Gen Intel(R) Core(TM) i5-13500 (20 logical) |
| RAM | 32 GB |
| Defender real-time | False |

WAN links are emulated by a local proxy (`bench/bench-proxy.ps1`) that adds RTT and/or
caps bandwidth without coupling the two.

## Efficiency by data type, channel and ping

Cells are EFFICIENCY = goodput / channel capacity - how much of the link we actually use.
About 100% means the link is saturated; ABOVE 100% means adaptive compression delivered more
original data than the wire physically carried; below 100% means we are bottlenecked off-link
(small files = receiver disk). Each bandwidth is shown at 0 ms and at 150 ms round-trip. Default
settings (4 parallel streams + adaptive compression). Channel capacities: 20 Mbit = 2.5 MB/s,
100 Mbit = 12.5 MB/s, 200 Mbit = 25 MB/s.

| data type | 20 Mbit | 20 Mbit +150ms | 100 Mbit | 100 Mbit +150ms | 200 Mbit | 200 Mbit +150ms |
|---|---|---|---|---|---|---|
| small files (10000 x 4 KB) | 92% | 88% | 69% | 59% | 40% | 33% |
| large, incompressible (4 MB files, random) | 96% | 92% | 94% | 84% | 81% | 76% |
| large, compressible (4 MB files, text 3.73x) | 252% | 216% | 290% | 209% | 270% | 169% |

On LAN (loopback) the link is not the bottleneck, so link-efficiency is not meaningful. For reference, raw LAN goodput on this machine: small files 7.5 MB/s, incompressible 79.3 MB/s, compressible 148.8 MB/s.

**How to read it:**
- *Incompressible* large files stay link-bound (high %): nothing to compress, so we push raw at close to the wire rate. The shortfall from 100% is per-bundle negotiation, which grows on faster links.
- *Compressible* large files go ABOVE 100% (data packs ~3.7x): adaptive compression sends fewer bytes, so more original data arrives per second than the wire could carry raw. On a fast LAN it would stay near raw - compressing there only costs CPU.
- *Small files* stay below 100%: they are bound by file creation on the receiver (NTFS metadata + antivirus), not the link. Bare create floor on this disk: 4.0s single-thread / 1.9s parallel for 10000 files.
- *Latency* costs little: one round-trip per ~10 MB bundle, not per file. The +150ms columns stay close to 0 ms; the gap only widens on the fastest links, where each transfer is so short that the few fixed round-trips are a visible slice of it.

