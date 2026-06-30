# Changelog

All notable changes to folder-transfer are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and the project
aims to follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Planned
- Block‑level delta (rsync‑style) for large, slowly‑changing files.
- VSS snapshot serving for zero‑downtime consistent database copies.
- Optional hash‑based integrity verification (`-Verify`).
- Optional server‑side transfer log for auditing.

## [0.18.0] — 2026-07-01

### Performance (Rust `ft`)
- **Switched the compressor from deflate to zstd** (vendored libzstd, **statically linked** — the
  release is still a single self-contained binary, no runtime DLL/.so). zstd dominates deflate on
  both ratio and speed: e.g. on log/JSON data ~8–9× vs deflate's ~5×.
- **Adaptive compression level by link speed.** A per-connection controller picks the highest zstd
  level whose compression stays at least `--compress-margin`× the link speed (default **1.6**), so a
  slow link compresses harder (more ratio → fewer bytes) and a fast link compresses lighter (down to
  zstd's ultra-fast negative levels, or raw if even those lose). The link rate is measured over each
  window's **wall-clock** (buffer-immune; per-block write time is not — a flush returns into the socket
  buffer instantly). Incompressible data is detected and sent raw (no expansion).
- **One shared level controller across all parallel streams.** Pooling every stream's measurements
  converges the level quickly; with the default 4 streams a single per-stream controller saw too
  little data to adapt. Measured: streams=4 over a 30 Mbit link climbs to ~level 10 (~9× on logs).
- Configurable via `--compress-margin <x>` (JSON `compressMargin`). Removed the deflate code and the
  `flate2` dependency.
- **Buffer-immune level signal (stops the level oscillating).** The level decision now uses the
  window's wall-clock to judge how far compression out-paces the link (`spare`), instead of summed
  per-block write time. Per-block write time is distorted by the socket send buffer — `--debug` logs
  showed it swinging the level wildly (3→14) and overshooting into a state where the compressor
  starved the link. The wall-clock signal is stable, so the level settles cleanly at the
  margin-correct point. Trade-off: on a >1 GB/s link / loopback the controller now compresses to match
  the *receiver's* rate even when raw memcpy would be faster there (it can't see the raw
  counterfactual); on any real WAN link this is a clear win, and `--no-compress` covers the fast-LAN
  edge. A 200 Mbit link with compressible text went from ~150% to ~460% efficiency over the 0.17 line.
- **`--debug` mode.** Logs important decisions — chiefly every adaptive-level change (new level, the
  `spare` and measured link rate, the realized ratio) and raw-mode toggles — to stderr **and** a debug
  log file (`ft-debug.log`, or `--debug-log <path>`), so you can watch what the controller does and
  share the log. Off by default; one atomic check when off. (The window-clock starts at the first
  block of each window, so the first `--debug` line shows a real link rate, not a startup artifact.)

### CLI / UX (Rust `ft`)
- **The window no longer closes before you can read the result.** When `ft` is launched into its own
  console (double-clicked, or from a launcher) it now waits for Enter at the end, so the final
  `sync DONE … bytes=… @ … MB/s` summary stays on screen. It only pauses when it owns the console and
  stdin is interactive — running from an existing shell, a pipe, or a script is unaffected. Force it
  from a launcher with `--pause`.
- **Serve configs self-upgrade.** Loading an older server config (e.g. one written before `streams` or
  `compressMargin` existed) appends the missing current options with their defaults and a one-line
  comment each, so the file shows everything you can tune and stays editable. Your existing values,
  comments, and layout are preserved (it only appends); the original is saved to `<config>.bak`. The
  rewrite is validated before it's written and is skipped once the config is current — a read-only or
  managed config is left untouched.
- **Latency: a fresh fetch no longer pays a round-trip per bundle / per large file.** When the
  destination is empty, the parallel client requests *stream mode* (`QSYNC STREAM`): the server sends
  every file at offset 0 without waiting for a per-bundle want-mask or a per-file offset reply (both
  are pure latency on a fresh pull). A re-sync / mirror (non-empty destination) keeps the round-trips,
  so resume and mirror-delete are unchanged.
- **Coalesced small-file writes.** A bundle's file bodies now flush in ~512 KiB batches instead of one
  flush per file, so thousands of tiny files ride full-size TLS records/TCP segments instead of runt
  segments — higher small-file throughput at every latency.
- **One fewer handshake on a parallel transfer.** The client used to open a throwaway probe connection
  just to learn the stream count, then drop it and reconnect N workers; it now reuses that connection
  as the first worker, so a parallel run does N handshakes, not N+1 (≈ one RTT less startup — ~10% on
  a small transfer over a 150 ms link).

### Notes
- Compressed blocks are now zstd **and** the parallel handshake gained `QSYNC STREAM`, so the wire
  format changed — both ends must run 0.18.0+ (Rust ↔ Rust only; the PowerShell edition is a separate
  tool and unaffected).

## [0.17.0] — 2026-06-30

### Performance (Rust `ft`)
- **Small-file bundles are now compressed adaptively** (previously always sent raw). Each file in a
  bundle uses the same per-connection raw/deflate decision as large files, so many compressible
  small files no longer go uncompressed — measured ~2.6–2.8× fewer bytes on the wire for compressible
  small/medium files, which is a similar speed-up on a link-bound link.

### Notes
- The bundle wire format changed (small files are now framed `Z`/`R`/`-1` like large-file blocks),
  so both ends must run 0.17.0+. Rust ↔ Rust only; PowerShell interop is unaffected (separate tool).
- A pipelined / multi-core / adaptive-level compression experiment was prototyped and then dropped:
  benchmarking (deterministic on-the-wire bytes) showed it gave no link-bound gain and the
  adaptive-level heuristic was counterproductive (raising the level slowed deflate, which made the
  A/B decision fall back to raw and send *more* bytes). Only the bundle-compression win was kept.

## [0.16.0] — 2026-06-29

### Added (Rust `ft`)
- **Embeddable library / C‑ABI shared library** (`ft.dll` / `libft.so`) so file transfer can be
  driven from your own code (.NET via P/Invoke, C/C++, etc.) without spawning the CLI. Exposes
  `ft_get` (pull a folder), `ft_serve_start` / `ft_serve_wait` (serve in the background; the token
  and certificate fingerprint are returned immediately to hand to the receiver), and
  `ft_last_error`. A C header (`ffi/ft.h`) and a ready .NET binding (`ffi/FolderTransfer.cs`) ship
  in the Rust archives. The crate is now built as `rlib` + `cdylib`; the CLI binary is unchanged.

## [0.15.3] — 2026-06-29

### Fixed (Rust `ft`)
- **No more silent hang on a dropped connection.** The client now sets a read timeout, so if the
  link stalls or drops it fails fast with a clear message — `connection to the server was lost
  before the sync finished` + `sync INCOMPLETE … re-run to resume` — and exits non‑zero, instead of
  sitting forever at the last progress line. The server sends `PING` keepalives while it waits for
  its file‑walker so the timeout never fires on a slow scan (the client ignores `PING`).
- The final line is now always printed: `sync DONE …` on success, `sync INCOMPLETE …` on a drop.

## [0.15.2] — 2026-06-29

> Supersedes 0.15.1 (an incomplete pre‑release): the items below are all built into the 0.15.2
> binaries.

### Added (Rust `ft`)
- **No subcommand needed — just point `ft` at a folder or a JSON**, PowerShell‑style. The first
  positional argument is auto‑routed: a folder or a server config (`folders`/`folder`) starts the
  **server**; a connection file (`fingerprint`/`token`/`server`) starts the **client** (an optional
  second positional is the destination). `ft serve` / `ft get` still work explicitly. Examples:
  `ft server.example.json`, `ft C:\data`, `ft ft-download-Name.json D:\incoming`.
- **`server.example.json`** shipped in the Rust archives: a ready, fully‑commented server config
  (same keys as `ft-server.ps1`) that runs out of the box (`ft server.example.json`).
- The receiver instructions the server prints now put the **destination as the LAST argument**
  (`ft ft-download-<name>.json <DEST>`), so you just append the path instead of editing the
  middle of a long command.
- **Smaller connection file / command.** The connection file (and the printed one-liner) now carry
  only `server`, `port`, `token`, `fingerprint`. The server pushes the ignore patterns and the
  stream count to the client right after connecting, so the client picks the right mode and mirror
  rules automatically — nothing extra to copy. (`--ignore`/`--streams` still override if given.)
- **Interactive destination.** If `--to`/`<DEST>` is omitted, the client asks where to save
  (Enter = current folder, shown as the default) — so you can just copy-and-run the printed
  command, no file and no path to edit. The server's printed hint now leads with that command.
- **Live progress on both sides.** During a transfer each side prints one throttled, **aggregated**
  line (~every 1.5 s) — `N files, X MB in MM:SS @ Y MB/s` — summed across all parallel streams (no
  more per-stream numbers jumping around), and including **elapsed time**. Mid-file ticks mean even
  one big file shows movement; the final summary shows the total time. (Fast LAN transfers finish
  before the first tick, so they stay quiet.)
- **Highlighted receiver command.** When the server starts it prints the command to copy in a
  framed, cream-coloured block (ANSI colour when stderr is a terminal — VT is enabled on the
  Windows console; plain text when redirected) so it's obvious what to copy to the other machine.

### Fixed (Rust `ft`)
- Empty‑string config values (`"clientOut": ""`, `"serverHost": ""`, `"allowIp": ""`) are now
  treated as "not set" (matching the PowerShell tool), instead of as a literal empty path / empty
  IP. Previously an empty `clientOut` aborted startup with `os error 3`, and an empty `allowIp`
  would have rejected every client.
- Filesystem errors now name the offending path (e.g. the client‑out folder or an unresolved
  shared folder) instead of a bare `os error 3`.

## [0.15.0] — 2026-06-29

### Added
- **Rust port (`ft`).** A single self‑contained, dependency‑free binary that runs on **Windows
  and Linux**, wire‑compatible with the PowerShell tool. Two subcommands: `ft serve`
  (== ft‑server.ps1) and `ft get` (== ft‑client.ps1), with both single‑stream (`SYNC`) and
  parallel (`QSYNC`, `--streams N`) modes, TLS 1.2/1.3 via rustls with SHA‑256 certificate
  pinning, adaptive per‑block compression (raw deflate), JSONC config with CLI‑wins merge,
  ignore‑pattern parity, `--allow-ip` enforcement, and the intrinsic mirror delete. The
  PowerShell scripts remain the reference implementation.
- **Cross‑platform release artifacts.** Each release now also ships `ft-<ver>-x86_64-windows.zip`
  and `ft-<ver>-x86_64-linux.tar.gz` (fully static musl) alongside the PowerShell
  `folder-transfer-<ver>.zip`. A CI workflow (`.github/workflows/rust-release.yml`) builds both
  Rust targets on a tag; `release.ps1` gained an opt‑in `-WithRust` step (the PowerShell ZIP step
  is unchanged).
- **Generated client (Rust).** `ft serve` writes a `download-scripts/ft-download-<name>.json`
  connection file and prints a ready‑to‑run `ft get …` command; `ft get --config <that>.json`
  consumes it.
- **Benchmarks & cross‑OS validation** for the Rust binary in
  [BENCHMARKS-rust.md](BENCHMARKS-rust.md): ~1.5× raw LAN goodput on incompressible data vs
  PowerShell, 94–99% saturation on fast links, and byte‑identical Windows↔Ubuntu transfers with
  mtime‑ticks preserved.

### Notes
- The Windows firewall opening stays Windows‑only / best‑effort; on Linux it is a no‑op.
- No changes to the PowerShell scripts (other than the additive `release.ps1 -WithRust`).

## [0.14.0] — 2026-06-28

### Added
- **Adaptive compression.** Compression is no longer all‑or‑nothing: the server measures, per
  connection, how fast it actually moves data raw vs compressed and only compresses when that is at
  least **25% faster** end‑to‑end. On a fast LAN it sends raw (compression there is pure CPU cost
  and roughly halved throughput); on a slow or compressible link it compresses and effective
  throughput rises by about the compression ratio. Fully automatic — `-NoCompress` still forces it
  off. The wire format is unchanged (the receiver already decodes mixed raw/compressed blocks).
- **Benchmark suite (`bench/`).** `bench\bench.ps1` builds corpora, drives real transfers through a
  bandwidth/latency‑emulating proxy (`bench\bench-proxy.ps1`), and writes `BENCHMARKS.md`; method
  and reproduction steps in `bench/README.md`. A results table is shown on the project home page.

### Changed
- **Hot path moved from cmdlets to .NET** on both sides (file existence/size/mtime checks, directory
  creation, timestamps, line timing) — a measurable speed‑up on many‑small‑file transfers, in both
  single‑stream and parallel modes.

### Fixed
- **Destination on an 8.3 short path synced nothing.** If `-ToFolder` resolved to a short path
  (e.g. `C:\Users\RUNNER~1\...`), the receiver's path‑safety check compared a short root against
  expanded target paths and rejected **every** file as unsafe. The destination is now normalised the
  same way as the targets.
- **Files >= 2 GB in the raw (uncompressed) path** crashed with an Int32 overflow in the read loop;
  the byte count is now 64‑bit throughout.
- **Small parallel jobs could skip the mirror delete.** A stream that lost the connect race and
  claimed no work was treated as a failed stream, marking the whole run incomplete so nothing was
  deleted; such idle streams are now benign and the reconciliation pass runs as intended.

## [0.13.0] — 2026-06-28

### Changed
- **Parallel mode now shards by fine‑grained work units (bundles / large files), not whole
  folders.** A single producer lazily walks the source and feeds a bounded shared queue; the N
  connections pull units until it's drained. Result: even **one giant folder now spreads across all
  streams** automatically — no need to split it into separate `folders` entries for balance.
- **Mirror is exact again in parallel mode.** All streams record what they received into one shared
  set; after every stream finishes cleanly, the receiver does a single reconciliation pass and
  deletes any local file the source no longer has — **including files under a whole top‑level folder
  removed on the source**. This removes the v0.12.0 limitation where vanished top‑level folders were
  not pruned in parallel mode. If any stream drops, the run is treated as incomplete and nothing is
  deleted that time (re‑run to complete). As before, the mirror deletes files, not directories.

### Fixed
- Parallel server handlers initialise their progress timers before sending, fixing an
  `op_Subtraction` error that could abort sends (and write 0‑byte files) under the new unit model.

## [0.12.0] — 2026-06-28

### Added
- **Parallel streams (`-Streams <n>`, default 4).** Transfers now use several TCP connections at
  once, which lifts the single‑connection `window ÷ RTT` throughput ceiling — a large speed‑up on
  high‑latency links. The sender queues the shared folders and each connection pulls a whole folder
  at a time (runspace per connection on both sides), so the streams self‑balance. Small‑file
  bundling and adaptive compression still apply within each stream. Set `-Streams 1` (or
  `"streams": 1`) for the classic single‑stream behavior; `-Cutover` forces single‑stream.

### Notes / limitations
- Balance is **per shared folder** — a single giant folder is handled by one connection and is not
  split across streams. List its subfolders as separate `folders` entries to parallelize one big
  tree.
- In parallel mode, a **whole top‑level folder deleted on the source is not auto‑removed** on the
  receiver (files removed *inside* a folder still are). This keeps the per‑folder mirror simple and
  always correct; run once with `-Streams 1` to prune vanished folders, or delete them by hand. The
  server and client print this caveat at startup when `-Streams > 1`.

## [0.11.3] — 2026-06-27

### Changed
- **Adaptive compression — try, and back off when it doesn't help.** For large files the per‑block
  compressor now measures each 1 MB block: if it doesn't shrink (≥ 5 %) the block is sent raw, and
  after a few poor blocks the server stops compressing for a while before probing again. So no CPU
  is wasted compressing data that doesn't compress (e.g. an unknown but already‑compressed format),
  and incompressible data is never expanded on the wire. Known already‑compressed extensions still
  skip compression up front.

## [0.11.2] — 2026-06-27

### Changed
- **Bundle by size, and bundle bigger files.** Small‑file bundling now batches files up to **1 MB**
  (was 64 KB) and flushes a bundle once it has accumulated **~10 MB** of files (was a flat
  1024‑file count), with a 4096‑file safety cap for huge numbers of tiny files. Grouping by size
  means each round‑trip carries a meaningful chunk of data regardless of individual file sizes.
  (Bundled files are sent raw; files > 1 MB still go individually and compressed.)

## [0.11.1] — 2026-06-27

### Changed
- **Bundle size raised 256 → 1024 small files.** Fewer round‑trips for trees with lots of small
  files over a high‑latency/WAN link (e.g. ~1M tiny files now take ~4× fewer exchanges). Memory
  is just per‑bundle metadata, so it stays negligible.

## [0.11.0] — 2026-06-27

### Changed
- **No up‑front file count — the transfer starts immediately.** Previously the server walked the
  whole tree once before sending, to compute the "x of N" total for the ETA; on a big tree or a
  slow disk that was a silent pause before anything happened, plus a second full enumeration. That
  pre‑walk is gone: sending begins at once and the progress line now shows files done, data moved
  and speed (no "of N" / ETA). Net: faster start and no duplicate walk. Progress still updates
  mid‑file so large files don't look frozen.

## [0.10.4] — 2026-06-27

### Changed
- **Simplified `--help`.** It now leads with the folder‑or‑config entry and just the few common
  options (`-Once`, `-AllowIp`, `-ServerHost`, `-Ignore`), with the rest under a one‑line
  **ADVANCED** pointer (and a note that everything can also go in the JSON config). No behaviour
  change — all options still work.

## [0.10.3] — 2026-06-27

### Changed
- **A `.json` first argument is auto‑detected as the config** — just run
  `folder-transfer.bat sync.json` instead of `folder-transfer.bat -Config sync.json` (the
  positional argument is now either a folder or a `.json` config; `-Config` still works).

## [0.10.2] — 2026-06-27

### Docs
- Brought the documentation up to date with the current protocol and features: rewrote
  **ARCHITECTURE.md** (the `T`/`D`/`B`/`F` wire protocol, small‑file bundling, on‑the‑fly
  compression, multiple folders, path‑aware ignore + empty‑dir recreation, JSON config) and
  refreshed README / CONTRIBUTING / SECURITY accordingly.

## [0.10.1] — 2026-06-27

### Changed
- **Ignored directories are recreated empty on the receiver** (with their subdirectory tree),
  instead of being skipped entirely. Their files are still not transferred — but the folders now
  exist, because some software won't start without them. (New one‑way `D <rel>` protocol message;
  the empty dirs aren't touched by the mirror.)

## [0.10.0] — 2026-06-27

### Added
- **Automatic small-file bundling — big speedup for many small files over a WAN.** Files
  ≤ 64 KB are batched (up to 256 per bundle) and exchanged in **one round-trip per bundle**
  instead of one per file: the server sends a manifest, the client replies with a want‑mask, then
  only the needed files stream back. The delta is preserved (unchanged files are still skipped),
  the mirror still works (bundled files count as offered, so they're not deleted), and a locked
  file in a bundle is skipped gracefully. Large files (> 64 KB) are unchanged — sent one at a
  time, compressed, with mid‑file progress. No client setup; the tool decides automatically.

## [0.9.3] — 2026-06-27

### Changed
- **Disabled Nagle's algorithm (`TCP_NODELAY`) on both sockets.** For many small files over a
  WAN/internet link the bottleneck is per‑file round‑trips, and Nagle + delayed‑ACK was adding
  ~40–200 ms to each one. Turning it off noticeably speeds up small‑file transfers. (The protocol
  is still one round‑trip per file, so very small files over a high‑latency link remain
  latency‑bound — a pipelined protocol would be the next step.)

## [0.9.2] — 2026-06-27

### Changed
- **Progress updates during a single file, not just between files.** Both server and client now
  refresh the progress line (~every 2 s) while a large file is streaming, so the megabytes and
  speed keep climbing instead of looking frozen on a multi‑GB file.
- **Default `-StallTimeout` raised 120 → 300 s.** A connected client now has to be silent for
  longer before the server aborts the session, which avoids killing a transfer that is actually
  progressing over a slow/WAN link. For very large files raise it further (e.g. `1200`) via
  `-StallTimeout` or `"stallTimeout"` in the config.

## [0.9.1] — 2026-06-27

### Fixed
- **Destination path with a trailing backslash broke the client.** Running
  `ft-download-X.bat t:\bridge\` baked `-ToFolder "t:\bridge\"`, where the `\"` escaped the
  quote and shifted the remaining arguments — so `-Fingerprint` never reached the client and it
  failed with *"-Fingerprint is required"*. The generated client now strips trailing backslashes
  from the destination (keeping a bare drive like `T:` → `T:\`).

## [0.9.0] — 2026-06-27

### Added
- **Comments in the JSON config.** The `-Config` file may now contain `//` line and `/* */`
  block comments (string‑aware, so paths are untouched). `sync.example.json` is now fully
  commented — every parameter has a short explanation inline. Genuine JSON errors (a single
  backslash, a trailing comma) are still reported, not auto‑fixed.

## [0.8.2] — 2026-06-27

### Changed
- **`sync.example.json` is now a relatable "move to a new PC" example** — copies Documents /
  Pictures / Desktop / Downloads and shows the ignore syntax by example, including both `*`
  (`*.tmp`, `~$*`) and `**` (`**/node_modules/`, `**/cache/`). README example matched.

## [0.8.1] — 2026-06-27

### Changed
- **Config parsing is strict JSON — no auto‑correction.** Forward slashes (`C:/path`) and doubled
  backslashes (`C:\\path`) both work (valid JSON); a single backslash or a trailing comma is now
  reported as an invalid config (with a short tip) instead of being silently "fixed". Auto‑repair
  could mask real mistakes, so it was removed.

## [0.8.0] — 2026-06-27

### Added
- **Path-aware ignore patterns.** A pattern containing `/` is now matched against the relative
  path (anchored at the shared‑folder name), so `AdminEye/Reports/` skips just that subfolder and
  `*/cache/` / `**/cache/` target a depth or any depth. Patterns without `/` still match a name at
  any depth. `*` and `?` stay within a segment; `**` spans `/`. Applied identically on the client
  so the mirror never deletes ignored paths.
- **Forgiving JSON config.** Paths may use single backslashes (`C:\Data`), doubled (`C:\\Data`) or
  forward slashes — all parsed correctly — and a **trailing comma** after the last list item is
  tolerated. A folder path with a trailing slash now resolves correctly too.
- **`sync.example.json` lists every parameter** (folders, ignore, compress, cutover, once, allowIp,
  serverHost, clientOut, port, idleSeconds, stallTimeout, noFirewall) so the full format ships in
  the release.

## [0.7.2] — 2026-06-27

### Changed
- `sync.example.json` now shows **Windows backslash paths** (doubled, `H:\\Data\\ProjectA`) since
  this is a Windows tool — forward slashes still work too. README example updated to match.

## [0.7.1] — 2026-06-27

### Added
- **`sync.example.json` ships with the tool** (and in the release ZIP) — a ready‑to‑edit sample
  `-Config` file showing every key (`folders`, `ignore`, `compress`, `cutover`, `once`, …) so
  the format is discoverable without digging through the README.

## [0.7.0] — 2026-06-27

### Added
- **On-the-fly streaming compression, on by default.** File payloads are now Deflate-compressed
  as they stream, in constant-memory 1 MB blocks (no whole-file buffering). It is **smart**:
  already-compressed/encrypted types (`.zip .7z .gz .jpg .png .mp4 .pdf .docx …`) and tiny
  files are sent raw, so no CPU is wasted where it wouldn't help. The server reports the saving,
  e.g. `pass 1 done … 24,785,734 bytes (3,306,207 on wire, 87% saved by compression)`.
  Turn it off with **`-NoCompress`** or JSON `"compress": false`. Purely server-driven and
  per-file (each transfer is tagged raw/compressed), so the receiver needs no extra setup.

## [0.6.0] — 2026-06-18

### Added
- **Multiple folders, ignore patterns, and a JSON config.** Share several folders in one run
  and skip paths you don't want (big log dirs, temp files):
  - `-Config <file.json>` — put it all in JSON: `folders` (list), `ignore` (list), and any of
    the usual options (`port`, `allowIp`, `serverHost`, `cutover`, `once`, …). Use forward
    slashes or escaped `\\` in paths. Command‑line options override JSON.
  - `-Ignore <list>` — ignore patterns on the command line (comma/semicolon separated).
  - **Pattern rules:** matched against each path‑segment **name**, wildcards `*` and `?`,
    case‑insensitive, at any depth. A **trailing `/` means directory‑only** (`log/` skips
    folders named `log` but keeps a file named `log`); without it, both. A matched directory is
    pruned whole.
  - Ignored content is **never transferred and never deleted** on the receiver (patterns are
    baked into the generated client so its mirror step skips them).
  - Each shared folder still arrives under its own name (`<dest>\Bars\…`, `<dest>\Ticks\…`).

### Docs
- README trimmed to the essentials with a **Contents** (table of contents) at the top for quick
  jumping; deeper detail stays in [ARCHITECTURE.md](ARCHITECTURE.md).

## [0.5.1] — 2026-06-18

### Changed
- The progress line now also shows **transfer speed (MB/s)** and an **ETA**, e.g.
  `progress: 4054/6000 (1946 left) - fetched 4054, unchanged 0, 12.3 MB @ 215.0 MB/s, ETA 00:00:09`.
  The client's final summary reports total elapsed time and average speed. (Speed is measured
  over the last interval; ETA is estimated from the file‑processing rate, so it is a guide, not
  a guarantee — a few very large files left can make it optimistic.)

## [0.5.0] — 2026-06-18

### Added
- **Live progress during a sync, on both sides.** The server counts the files in the tree at
  the start of each pass and both server and client now print a throttled progress line
  (about every 2 seconds): how many files are done, how many are left, how many were fetched
  vs unchanged, and how much data has moved — e.g.
  `progress: 4054/6000 (1946 left) - fetched 4054, unchanged 0, 12.3 MB`. The count is sent
  to the client over the wire (a `T <n>` line) so it can show "x of N" too. Previously a long
  transfer printed nothing between the start and end of a pass.

## [0.4.1] — 2026-06-18

### Fixed
- **A locked/in-use file no longer aborts the whole sync.** The server now opens each file
  with a permissive share mode (`FileShare.ReadWrite, Delete`), so files another process holds
  open — e.g. a **live database's data/log files during cutover pass 1** — can be read and
  transferred instead of throwing *"The process cannot access the file … because it is being
  used by another process"* and killing the session. If a file is still locked **exclusively**,
  the server logs it and tells the client to skip it for that pass (the client keeps its current
  copy — no truncation, no wrongful delete) and carries on; in cutover, pass 2 (after the DB is
  stopped) picks it up consistently.

## [0.4.0] — 2026-06-18

### Added
- **Interactive server prompts.** Running `folder-transfer.bat` with no arguments (e.g. by
  double‑clicking) now asks what to do instead of printing help: (1) which folder to share /
  sync, and (2) which mode — single‑phase or cutover. This mirrors the client, which already
  asks for its destination. Explicit command‑line arguments still work and skip the questions;
  `--help` / `-h` / `/?` still show help.

### Changed (packaging)
- The release ZIP now contains a top‑level **`FileTransfer\`** folder, so unzipping creates
  that folder ready to use — no need to choose a destination folder when extracting.

### Docs
- Expanded the Mark‑of‑the‑Web guidance: the "Open File – Security Warning / Unknown
  Publisher" dialog comes from files extracted from a downloaded ZIP. **Unblock the `.zip`
  before extracting** (right‑click → Properties → Unblock) to clear it from all files at once.

## [0.3.0] — 2026-06-18

### Added (usability)
- **Client asks for the destination** when you run it without one (e.g. by double‑clicking):
  it prompts for the target folder and uses the **current folder** if you just press Enter.
- **Windows stay open at the end.** Both the server launcher (`folder-transfer.bat`) and the
  generated client `.bat` pause after finishing (success *or* error) so you can read the final
  status before the console closes.

### Changed (rebrand)
- Renamed the project to **folder-transfer** (from the internal working name "OTFT"). The
  sender is now `folder-transfer.bat` → `ft-server.ps1` (with `ft-client.ps1` alongside);
  the generated receiver file is `ft-download-<name>.bat`. Internal identifiers moved too:
  TLS certificate subject `CN=ft-onetime`, embedded‑client marker `#FTPSBODY#`, cutover flag
  `ft-cutover.go`. Behaviour is unchanged — names only.

### Changed (packaging)
- The **generated receiver client is one self‑contained `.bat`** again (plain‑text client
  embedded after a marker; no separate `ft-client.ps1` to carry, no base64). The sender
  stays thin (`folder-transfer.bat` → `ft-server.ps1`). Asymmetric on purpose: the source box runs
  fixed/allow‑listable scripts, while the file that travels is a single convenient one.

### Changed (simplification)
- **Two modes only:** default = single‑phase sync; `-Cutover` = two‑phase sync. The old
  one‑time `PULL` mode is gone — every transfer is now a sync.
- **Sync always mirrors.** The receiver is always made an exact copy (changed/new fetched,
  removed deleted). The `-Mirror` flag is **removed** (it was a pointless toggle); the
  client derives the subtree to mirror from the paths, so the protocol `ROOT` line is gone.
- **Token is always auto‑generated** and baked into the client. The `-Token` parameter is
  **removed** — you never set it, and client auth is always on.
- Removed the standalone `-Sync` flag (sync is the default behaviour now).

### Fixed
- **Mirror in cutover:** the "seen files" set is now reset per pass and the mirror runs only
  on a clean finish, so a file deleted on the source **between cutover pass 1 and pass 2** is
  correctly removed on the client, and a dropped connection never triggers wrong deletions.

## [0.2.0] — 2026-06-17

### Added
- **Mode 2 — delta sync** (`-Sync`): the client re‑fetches only changed/new files,
  detected by size + last‑write‑time; unchanged files are skipped without reading them.
  Source mtime is preserved on written files so subsequent syncs compare correctly.
- **Mirror** (`-Mirror`): the client deletes local files that were removed from the source,
  scoped to the shared folder's subtree.
- **Database cutover** (`-Cutover`): two passes in one session — pass 1 while the database
  is live, then the server pauses for the operator to stop the database (keypress or
  `ft-cutover.go` flag file, with PING keepalives), then pass 2 transfers only the delta.
  Degrades gracefully to a re‑run if the paused connection drops.

### Changed
- File offers now include the source mtime: `F <size> <mtime> <relpath>` (Mode 1 ignores
  the mtime field).

### Packaging
- **Thin `.bat` launchers** (final design). `folder-transfer.bat` is a one‑line wrapper that runs the
  adjacent `ft-server.ps1`; the generated `ft-download-<name>.bat` runs the adjacent
  `ft-client.ps1` (copied next to it). **No temp extraction, no base64, no embedding/
  polyglot** — the scripts are fixed, readable files that can be AppLocker/WDAC‑allow‑listed
  and code‑signed, which suits hardened hosts. (An earlier single‑file polyglot build was
  dropped for this reason; `build.ps1` and the old `ft-server.bat` wrapper are removed.)
  The receiver now gets two files: the downloader bat **and** `ft-client.ps1`.
- README gained a **Security & antivirus** section: thin‑launcher rationale, and the real
  PowerShell‑side prod gates (GPO execution policy, WDAC/AppLocker Constrained Language
  Mode, EDR, admin for firewall), plus Mark‑of‑the‑Web.

### Diagnostics
- Timestamped server log with the client `IP:port`, session numbers, per‑pass file/byte
  counts, and how each session ended (clean vs dropped, with duration).
- `-StallTimeout` (default 120s): a connected client that sends nothing for that long is
  aborted and the server keeps listening. `-IdleSeconds` now explicitly governs only the
  wait **between** connections.

## [0.1.0] — 2026-06-17

First functional release. Verified end‑to‑end on Windows 11 over loopback.

### Added
- One‑time folder server (`ft-server.ps1` + `ft-server.bat`) and client engine
  (`ft-client.ps1`), pure PowerShell / .NET, no install required.
- TLS 1.2 transport via .NET `SslStream` with an ephemeral self‑signed certificate that
  is removed on exit.
- Certificate fingerprint pinning on the client (anti‑MITM).
- Optional shared `-Token` and optional `-AllowIp` client allow‑list.
- Streaming `PULL` protocol: lazy directory walk, constant memory, works with very large
  trees; per‑file resume by byte offset.
- Offset‑based file requests (client never sends a path) — directory traversal and
  Windows reserved device names are impossible by construction.
- Generated client `.bat` with connection details baked in, written under
  `download-scripts/`, one per shared folder.
- Mandatory destination on the client; shared folder is recreated by name inside it
  (`<dest>\<FolderName>\…`).
- Firewall opened by default for the transfer and removed on exit (scoped to `-AllowIp`
  when set); `-NoFirewall` to opt out; graceful warning when not elevated.
- One‑shot (`-Once`) and idle‑timeout (`-IdleSeconds`) auto‑shutdown; no service, user, or
  global config touched.
- Positional, required folder argument; case‑insensitive parameters; `--help` / `-h` /
  `/?` support.
- Documentation: `README.md`, `ARCHITECTURE.md`.

[Unreleased]: https://github.com/lotgon/folder-transfer/compare/v0.11.3...HEAD
[0.11.3]: https://github.com/lotgon/folder-transfer/compare/v0.11.2...v0.11.3
[0.11.2]: https://github.com/lotgon/folder-transfer/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/lotgon/folder-transfer/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/lotgon/folder-transfer/compare/v0.10.4...v0.11.0
[0.10.4]: https://github.com/lotgon/folder-transfer/compare/v0.10.3...v0.10.4
[0.10.3]: https://github.com/lotgon/folder-transfer/compare/v0.10.2...v0.10.3
[0.10.2]: https://github.com/lotgon/folder-transfer/compare/v0.10.1...v0.10.2
[0.10.1]: https://github.com/lotgon/folder-transfer/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/lotgon/folder-transfer/compare/v0.9.3...v0.10.0
[0.9.3]: https://github.com/lotgon/folder-transfer/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/lotgon/folder-transfer/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/lotgon/folder-transfer/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/lotgon/folder-transfer/compare/v0.8.2...v0.9.0
[0.8.2]: https://github.com/lotgon/folder-transfer/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/lotgon/folder-transfer/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/lotgon/folder-transfer/compare/v0.7.2...v0.8.0
[0.7.2]: https://github.com/lotgon/folder-transfer/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/lotgon/folder-transfer/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/lotgon/folder-transfer/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/lotgon/folder-transfer/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/lotgon/folder-transfer/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/lotgon/folder-transfer/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/lotgon/folder-transfer/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/lotgon/folder-transfer/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/lotgon/folder-transfer/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lotgon/folder-transfer/releases/tag/v0.2.0
[0.1.0]: https://github.com/lotgon/folder-transfer/releases/tag/v0.1.0
