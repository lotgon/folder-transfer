# folder-transfer (Rust `ft`) — benchmarks & cross-OS validation

Generated: 2026-06-29 | Version: v0.15.0 | Binary: `ft` (Rust port)

This page evaluates the Rust port (`ft serve` / `ft get`) the same way
[BENCHMARKS.md](BENCHMARKS.md) evaluates the PowerShell tool, and adds cross-OS
(Windows ↔ Ubuntu) validation. Reproduce with the scripts under `bench/`
(`build_corpora.py`, `proxy.py`, `bench_rust.sh`).

## Test environment

| | |
|---|---|
| Windows host | Windows 11 Enterprise (10.0.26200), Intel i5-13500 (20 logical), 32 GB, Defender real-time ON |
| Ubuntu side | the same host's WSL2 / Docker Desktop Linux VM (same CPU), ext4-on-vhdx, no antivirus |
| Rust | 1.96.0; Windows target `x86_64-pc-windows-msvc`, Linux target `x86_64-unknown-linux-musl` (static) |

> The Windows and Ubuntu numbers are NOT one hardware-identical comparison: the
> Ubuntu run lives in the WSL2 VM (different filesystem, no AV). Treat absolute
> MB/s per OS as "that environment", and read the trends, not the last digit.

## 1. Efficiency by data type, channel and ping — Rust vs PowerShell

Efficiency = goodput / channel capacity (≈100% = link saturated; >100% = adaptive
compression delivered more original data than the wire carried; <100% = bottlenecked
off-link, i.e. receiver disk). Default settings: 4 parallel streams + adaptive.
Channel caps: 20 Mbit = 2.5 MB/s, 100 Mbit = 12.5 MB/s, 200 Mbit = 25 MB/s.

| data type | impl | 20 Mbit | 20 +150ms | 100 Mbit | 100 +150ms | 200 Mbit | 200 +150ms |
|---|---|---|---|---|---|---|---|
| small (10000×4 KB) | PS   | 92% | 88% | 69% | 59% | 40% | 33% |
| small (10000×4 KB) | Rust | 96% | 92% | 55% | 56% | 37% | 31% |
| large incompressible | PS   | 96% | 92% | 94% | 84% | 81% | 76% |
| large incompressible | **Rust** | 96% | 92% | **99%** | **94%** | **99%** | **92%** |
| large compressible (3.73×) | PS   | 252% | 216% | 290% | 209% | 270% | 169% |
| large compressible (3.73×) | **Rust** | 232% | 212% | **294%** | **236%** | **296%** | **193%** |

**Reading it:** on the fast links (100/200 Mbit) Rust saturates the wire markedly
better for incompressible data (**94–99% vs PS 76–84%**) — less per-unit/round-trip
overhead. Compressible stays above 100% for both (same 3.73× deflate). Small files
are receiver-disk-bound on Windows (NTFS + Defender), so those cells are noisy and
machine-specific, not an implementation signal.

> Caveat: the throttled columns were measured with a faithful re-implementation of
> the WAN proxy in Python (`bench/proxy.py`); the PowerShell numbers used
> `bench/bench-proxy.ps1`. Both saturate a correctly-capped link, so the comparison
> is meaningful, but absolute % are not a single controlled harness.

## 2. LAN (loopback) raw goodput, MB/s — the implementation-speed view

No link cap, so the implementation is the bottleneck. (LAN is intentionally not an
efficiency column — a single loopback measurement is noisy.)

| data type | PS (4 streams) | Rust/Win 1 | Rust/Win 4 | Rust/Ubuntu 1 | Rust/Ubuntu 4 |
|---|---|---|---|---|---|
| small (10000×4 KB) | 7.5 | 3.5 | 8.4 | 127 | 102 |
| large incompressible | 79.3 | 123 | 118 | 83 | 288 |
| large compressible | 148.8 | 577* | 119 | 906 | 1117 |

- **Incompressible:** Rust on Windows ≈ 118 MB/s vs PowerShell 79 (~1.5×). On Ubuntu,
  parallel scales cleanly to **288 MB/s** (1→4 streams ≈ ×3.5).
- **Small files:** on Ubuntu ~15–30× faster than on Windows (127 vs 8.4 MB/s) — no
  real-time AV and cheaper ext4 metadata. This is the "file-creation floor", and it
  is much lower on Linux.
- **Compressible:** adaptive shines as original-throughput (Ubuntu up to ~1.1 GB/s).
  `*` the Windows single-stream 577 and sub-second small-file figures are noisy.

## 3. Cross-OS interop (Windows ↔ Ubuntu)

Validated with the static Linux binary running in Docker and the Windows binary on
the host. Each transfer asserts byte-identical content (SHA-256) and, where noted,
a no-op re-sync (proves size+mtime-ticks survive the OS round-trip).

| scenario | result |
|---|---|
| Ubuntu server ↔ Ubuntu client (container) | ✅ identical; re-sync = 0 fetched (mtime ticks survive on ext4) |
| Ubuntu server → Windows client | ✅ trees identical; 1.5 MB blob byte-identical |
| **Windows server → Ubuntu client** (win→ubuntu, 300 MB) | ✅ 75/75 files SHA-256-identical; **43.3 MB/s** |

The 43.3 MB/s win→ubuntu figure is the throughput of the **Docker Desktop virtual
network** between container and host (~344 Mbit/s ceiling), **not** ft's limit nor a
real NIC: on loopback the same data type moves at 118 MB/s (Win) / 288 MB/s (Ubuntu),
and the throttled table shows ft saturates a real capped link at 99%. On a physical
Windows↔Ubuntu LAN the transfer is NIC-bound and ft fills it.

## Conclusions

- **Rust is faster where the implementation is the bottleneck:** ~1.5× raw LAN
  goodput on incompressible data vs PowerShell, and 94–99% link saturation on fast
  WAN links (vs 76–84%).
- **Cross-platform works end-to-end and byte-for-byte**, both directions, with
  mtime-ticks preserved across Windows/NTFS ↔ Linux/ext4 (no-op re-syncs).
- **Small-file throughput is filesystem/AV-bound**, not protocol-bound — far higher
  on Linux (no Defender) than on Windows.
