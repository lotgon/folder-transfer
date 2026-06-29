# folder-transfer - Rust port specification (task / spec)

Status: **proposed** (not started). This document is the implementation brief for a
Rust rewrite of folder-transfer. It does NOT change the existing PowerShell tool.

## 1. Why and what

The current tool (`ft-server.ps1` / `ft-client.ps1`, driven by `folder-transfer.bat`)
is pure PowerShell 5.1 / .NET Framework and therefore **Windows-only**: it relies on
`New-SelfSignedCertificate` + the Windows cert store (SChannel), `New-NetFirewallRule`,
runspaces, and the system code page (hence the ASCII-only rule). We want the same tool
to run unchanged on **Windows and Linux (Ubuntu)** as a single self-contained binary
with no runtime to install.

The Rust port must be **wire-compatible** with the PowerShell version: a Rust server
must serve the PowerShell client and vice versa, in both single-stream and parallel
modes, including cutover, ignore patterns, mirror deletes, resume, and adaptive
compression. This lets users mix sides during migration (e.g. PowerShell sender on an
old Windows box, Rust receiver on Ubuntu).

### Deliverable in every release

Going forward a GitHub release ships **both** artifacts, side by side:

- the existing PowerShell ZIP `folder-transfer-<ver>.zip` (unchanged, built by `release.ps1`);
- Rust binaries, one archive per target (see section 9).

Neither replaces the other; the PowerShell scripts remain the reference implementation
and the source of truth for the protocol.

## 2. Non-goals

- No new features beyond protocol parity. Match behaviour first; improve later.
- No change to the PowerShell scripts as part of this work (other than, optionally,
  teaching `release.ps1` to attach the Rust binaries - see section 9).
- The Windows firewall opening is **Windows-only** and stays best-effort; on Linux it is
  a no-op (document it, don't fail).

## 3. Language / crates

- Edition 2021, MSRV pinned (latest stable is fine; record it).
- TLS: **rustls** (pure-Rust, no OpenSSL -> clean static binaries and trivial
  cross-compile). Must allow **TLS 1.2** (the PowerShell server forces TLS 1.2, so the
  Rust side MUST negotiate 1.2 when talking to it; Rust<->Rust may use 1.3).
- Self-signed cert: **rcgen** (in-memory, no cert store; subject `CN=ft-onetime`).
- Cert pinning: a custom `rustls` `ServerCertVerifier` that accepts ANY certificate
  whose DER SHA-256 equals the pinned fingerprint (hostname is ignored - mirrors the
  PowerShell `RemoteCertificateValidationCallback`).
- DEFLATE: **flate2** in **raw deflate** mode (RFC 1951), NOT zlib/gzip - .NET
  `DeflateStream` is raw deflate, and the wire format depends on it.
- SHA-256: **sha2**. RNG for the token: **rand** (or `getrandom`).
- CLI: **clap** (derive). JSON/JSONC config: **serde_json** + a comment-stripping pass
  (or `json5`/`jsonc-parser`); see section 7.4 for JSONC rules.
- Concurrency: std threads are sufficient (the parallel mode is a bounded producer +
  N handler threads). `tokio` is optional and not required.
- Glob -> regex (for ignore): **regex** (build anchored patterns exactly as the
  PowerShell `Convert-GlobToRegex` does).

## 4. The wire protocol (normative - extracted from the PowerShell code)

This section is the contract. Implement it byte-for-byte.

### 4.1 Transport

- Plain TCP. `TCP_NODELAY` on both ends (Nagle off - big win for many small files).
- TLS 1.2 over the socket. Server presents the self-signed cert; client pins its
  SHA-256 fingerprint and ignores the hostname. Client TLS SNI / target name is the
  literal string `ft-onetime`.
- The fingerprint is `lowercase hex of SHA-256(DER of the server cert)`, no separators.

### 4.2 Framing

Two interleaved layers on the **same** TLS stream:

- **Control lines**: UTF-8 text terminated by a single `\n` (0x0A). On read, a leading/
  trailing `\r` (0x0D) is ignored. Writers append `\n` and flush. A line read returning
  EOF with nothing buffered means "connection closed" (null).
- **Binary payloads**: raw bytes that immediately follow a length-announcing control
  line, with no extra framing. The reader consumes exactly the announced byte count
  straight off the stream, then resumes reading control lines.

All integers on control lines are decimal ASCII. `<mtime>` is **.NET `DateTime` ticks
of `LastWriteTimeUtc`** (100-nanosecond intervals since 0001-01-01T00:00:00Z). The Rust
side MUST convert to/from this representation exactly (see section 6.3) so that
size+mtime equality matches across implementations and re-syncs are no-ops.

### 4.3 Handshake (both modes)

```
client -> AUTH <token>          # <token> may be empty if the server has no token
server -> OK                    # success
server -> ERR auth              # bad token (server then drops / continues to reject)
```

Then the client issues exactly one of:

```
client -> SYNC                  # single-stream / classic mode
client -> QSYNC                 # parallel mode (one per connection)
```

A classic server (`-Streams 1`) accepts `SYNC` and answers anything else with
`ERR cmd`. A parallel server (`-Streams > 1`) accepts `QSYNC` and answers anything else
with `ERR cmd`. **The mode must match on both ends** - this is an existing,
deliberate constraint, not a bug. (Server and client are launched with matching
`-Streams` because the generated client bakes the server's value in.)

### 4.4 Item stream (shared by both modes)

After `SYNC`/`QSYNC` the server emits a sequence of **items**. Three item kinds:

**Empty directory** (used to recreate ignored/empty folders):
```
server -> D <rel>
```
`<rel>` is a path relative to the parent of the shared folder (so the shared folder's
own name is the first segment). Separators may be `\` or `/`; the receiver normalises.

**Bundle of small files** (files <= 1 MiB are batched; one round-trip for many):
```
server -> B <count>
server -> <size> <mtime> <rel>      # x <count> manifest lines, one per file
client -> <want-mask>               # exactly <count> chars, '1'=send me, '0'=skip
# then, for each file whose mask char is '1', IN ORDER:
server -> <len>                     # decimal byte length, or "-1" if the file is locked
server -> <len bytes>               # raw bytes (omitted when len = -1)
```
The mask is one ASCII char per manifest entry. A manifest line that fails to parse is
treated as a null entry: the client puts `'0'` for it. Bundled files are always sent
**raw** (no compression). `-1` means "couldn't open (locked) - keep your copy".

**Large file** (> 1 MiB, offered individually, supports resume + compression):
```
server -> F <size> <mtime> <rel>
client -> <offset>                  # decimal byte offset to START at; -1 (or unparsable) = skip
# if offset >= 0, server seeks to <offset> and sends the remainder as EITHER raw or adaptive:
#   RAW:
server -> R <nbytes>                # nbytes = remaining bytes after offset
server -> <nbytes bytes>
#   ADAPTIVE (per-block):
server -> Z
#   then a sequence of blocks, each ONE of:
server -> Z <clen> <rlen>          # <clen> deflate bytes that inflate to <rlen> bytes
server -> <clen bytes>
server -> R <rlen>                 # <rlen> raw bytes (this block did not compress)
server -> <rlen bytes>
#   ...ended by:
server -> E
```
Notes:
- The current PowerShell **client always sends `0`** for a large file it wants (full
  fetch, overwrite) and `-1` to skip. Server-side resume from a non-zero offset is
  supported by the protocol (`fs.Seek(offset)`), but the client does not request it.
  The Rust client MAY keep sending `0`; the Rust server MUST honour any non-negative
  offset.
- Within an adaptive (`Z`) stream the server can freely switch block-by-block between
  `Z` (compressed) and `R` (raw); the client decodes whichever each block announces.
  This is why adaptive compression needs **no** protocol flag - the receiver always
  handles a mixed stream.

### 4.5 Single-stream framing around the item stream (`SYNC`)

```
server -> T <total>                 # file count for the client's progress (0 = unknown)
# ... item stream (D / B / F) ...
server -> PASS-END                  # end of this pass
# non-cutover:
server -> DONE
# cutover (two-phase):
server -> PING                      # zero or more keepalives while the operator stops the DB
server -> GO                        # phase 2 begins
server -> T <total>                 # second pass
# ... item stream ...
server -> PASS-END
server -> DONE
client -> BYE
```
The client reads items until `PASS-END`. It then reads the next control line, skipping
any `PING`; `GO` -> run another pass; `DONE` -> mark sync OK and stop. `T <n>` may
appear at the start of each pass. Mirror deletes (section 5) happen after `DONE`.

### 4.6 Parallel framing around the item stream (`QSYNC`)

No `T`, no `PASS-END`, no `GO`, no cutover. The server interleaves items from a shared
work queue across all connected streams and signals end-of-work per connection with:
```
server -> NOUNIT
client -> BYE
```
Each stream loops: read item (`D`/`B`/`F`) until `NOUNIT`. All streams share one
"seen" set and one set of mirror roots; the single mirror pass runs once, after every
stream has finished cleanly (section 5).

A stream that loses the connect race (server already drained the queue and stopped
accepting) is **benign**: it claimed zero units, moved no data, and must NOT mark the
sync unclean or block the mirror. Only a stream that drops *after* receiving >= 1 unit
makes the run unclean.

## 5. Mirror (delete) semantics

The receiver makes the destination an exact mirror of the source:

- Destination layout: each shared folder is recreated by name under `<ToFolder>`, i.e.
  files land at `<ToFolder>\<rel>` where `<rel>` keeps the shared folder's own name as
  its first segment.
- Every offered file path (lowercased, absolute) is added to a `seen` set. Every
  offered top-level segment yields a mirror root `<ToFolder>\<top>`.
- **Safety prefix check**: every target path is `GetFullPath`-normalised and must start
  with `<ToFolder>\` (case-insensitive) or it is rejected as unsafe (reply `-1` / skip).
  This is what makes path traversal impossible - the client never trusts server paths
  blindly. (See section 6.2 for the Windows 8.3 caveat that this check tripped over.)
- After a **clean** finish only, walk each existing mirror root and delete any file not
  in `seen` and not matching an ignore pattern. In single-stream mode the `seen` set is
  cleared at the start of each pass, so the mirror reflects only the LAST pass (files
  deleted between cutover phase 1 and 2 get removed on the receiver too).
- An unclean finish deletes nothing ("re-run to complete").

## 6. Cross-platform behaviour details (must match)

### 6.1 Paths and separators
- Source paths on the wire use the sender's native separator; the receiver accepts both
  `\` and `/` and joins onto its own `<ToFolder>` with the local separator.
- `<rel>` is relative to the **parent** of the shared root, so the shared folder name is
  preserved on the receiver.

### 6.2 The 8.3 short-path bug (do not reintroduce)
On Windows, resolving a destination can yield an 8.3 short path (e.g.
`C:\Users\PETROS~1\...`) while target paths normalise to the long form
(`...\Petrosyan\...`), so the `StartsWith(rootPrefix)` safety check rejected every file.
The PowerShell client fixed this by normalising the destination with the SAME call used
for targets: `GetFullPath(Resolve-Path(dest))`. The Rust client MUST likewise canonicalise
the destination and all targets through one identical normalisation (e.g.
`std::fs::canonicalize`/`dunce::canonicalize`) before the prefix comparison, and compare
case-insensitively on Windows.

### 6.3 mtime = .NET ticks (critical for no-op re-syncs)
`<mtime>` is `LastWriteTimeUtc` in .NET ticks: 100 ns since 0001-01-01T00:00:00Z.
- Read: `unix_seconds = ticks / 10_000_000 - 62_135_596_800`; sub-second =
  `(ticks % 10_000_000) * 100 ns`.
- Write: invert. Set the file's mtime to this exact value after writing (the PowerShell
  client calls `SetLastWriteTimeUtc`). Equality is by `size && mtime_ticks` - if the Rust
  side rounds differently, every file looks changed and re-syncs copy everything.
- Match .NET's truncation (ticks are integers; don't introduce nanosecond drift).

### 6.4 Compression must be raw DEFLATE
Use flate2 raw deflate (`DeflateEncoder`/`DeflateDecoder`, NOT zlib/gzip wrappers).
The PowerShell side uses `CompressionLevel::Fastest`; pick the flate2 level closest to
.NET "Fastest" (roughly `Compression::fast()`), but any level interoperates - only the
**format** must be raw deflate.

### 6.5 Incompressible extensions (skip list)
Adaptive compression never attempts these (already compressed/encrypted). Replicate the
exact list from `ft-server.ps1` (`$script:IncompressibleExt`):
`.zip .7z .gz .tgz .rar .bz2 .xz .zst .lz4 .br .cab .msi .png .jpg .jpeg .gif .webp
.heic .tif .tiff .mp4 .mkv .mov .avi .wmv .webm .mp3 .aac .ogg .flac .m4a .pdf .docx
.xlsx .pptx .odt .ods .jar .apk .iso` (case-insensitive).

### 6.6 Adaptive compression decision (per connection, A/B)
Port the algorithm in `Send-LargeFile` exactly (it is the result of several wrong
attempts; do not "simplify"):
- Per-connection state: `CzRawBytes/CzRawSec` (raw mode: original bytes & seconds),
  `CzCmpBytes/CzCmpSec` (compressed mode: original bytes & seconds incl. deflate time),
  `CzRatio` (last block's `rlen/clen`), `CzSince` (blocks since last re-probe).
- Block size 1 MiB. `reprobe = 64`.
- `Tr = CzRawBytes/CzRawSec`, `Tc = CzCmpBytes/CzCmpSec` (original bytes per second).
- `incomp = CzRatio in (0, 1.05)` (last probe showed it doesn't shrink).
- Decision per block (only when block >= 256 bytes):
  - no compressed sample yet -> compress (seed `Tc`);
  - else no raw sample yet -> raw (seed `Tr`);
  - else `decided = (!incomp) && (Tc >= 1.25 * Tr)`; every `reprobe` blocks flip to the
    other mode to refresh its sample (`reprobeNow = true`), otherwise use `decided`.
- When compressing: if `clen < n*0.95` send `Z clen rlen`; else send it `R n` and record
  it as a RAW sample (write-time only). Update the matching counters with measured
  deflate+write or write time. Reset `CzSince` on a re-probe, else increment.
- The 25% margin (`1.25`) is the user-specified guard: compress only when it moves
  original data at least 25% faster than raw. On a fast link raw writes are ~instant so
  `Tr` is huge -> always raw (compression is pure CPU cost). Whole-file gate:
  `useCompress && remain >= 256 && !incompressible(ext)`.

### 6.7 Ignore patterns (must match the receiver's mirror too)
Port `Test-IgnoredRel` + `Convert-GlobToRegex` exactly:
- Patterns are `;`/`,`-separated. `\` is normalised to `/`. Trailing `/` => directories
  only.
- A body **without** `/` is a NAME pattern: matches ANY path segment, wildcards
  `*` (within a segment) and `?` (one char), case-insensitive (PowerShell `-like`).
- A body **with** `/` is a PATH pattern anchored at the root: `*` stays within a
  segment, `**` spans any depth, `?` one char; anchored `^...$`, case-insensitive.
- An item is ignored if the item or ANY ancestor directory matches. The server prunes
  ignored directories (sends `D <rel>` to recreate them empty so software that needs the
  folder still starts) and never sends their files. The client uses the SAME predicate
  so the mirror step never deletes ignored content.

### 6.8 Token
24 random bytes mapped onto the alphabet
`ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789` (note: no `O`/`o`/`l`/`1`).
Auto-generated by the server, baked into the generated client. The client sends it
verbatim in `AUTH`.

## 7. CLI parity

One binary, two subcommands (proposed name `ft`):
```
ft serve  ...   # == ft-server.ps1
ft get    ...   # == ft-client.ps1  (alias: download/fetch)
```

### 7.1 `ft serve` options (map 1:1 to `ft-server.ps1` param block)
`<folder>` (positional) | `--config <json>` | `--ignore <list>` | `--no-compress` |
`--streams <n>` (default 4) | `--port <n>` (default 8722) | `--allow-ip <ip>` |
`--idle-seconds <n>` (default 600) | `--stall-timeout <n>` (default 300) | `--once` |
`--server-host <addr>` | `--client-out <path>` | `--no-firewall` | `--cutover` |
`--help`. `--cutover` implies `--once` and forces `--streams 1`. `--streams < 1` -> 1.

### 7.2 `ft get` options (map 1:1 to `ft-client.ps1`)
`--server <addr>` (required) | `--port <n>` (8722) | `--token <s>` | `--to <path>`
(required; `ToFolder`) | `--fingerprint <hex>` (required) | `--ignore <list>` |
`--streams <n>` (1) | `--help`.

### 7.3 Behaviour parity
- Server: auto-generate token, mint in-memory self-signed cert, print
  `FINGERPRINT=<hex>`, listen on `0.0.0.0:<port>`, idle-timeout shutdown, `--once`
  exit-after-clean-session, cutover wait (keypress or a `ft-cutover.go` flag file, with
  `PING` keepalives ~every 15 s), stall timeout = read timeout on the TLS stream.
- Two server modes: classic loop (single connection at a time, `SYNC`) and parallel
  (bounded producer + N handlers, `QSYNC`); same shutdown rules ("all clients gone"
  grace, idle timeout when nobody ever connected).
- Client: connect, pin fingerprint, auth, run the pass(es), set mtimes, mirror-delete,
  print a final summary line; parallel client = N worker threads sharing `seen` +
  `mirrorRoots`, single mirror pass at the end.

### 7.4 Config (JSONC)
Accept a positional `.json` (or any existing file) as the config, same as the
PowerShell server. Support `//` and `/* */` comments (string-aware stripping, exactly
like `Remove-JsonComments`). Keys: `folders[]`/`folder`, `ignore[]`, `port`, `allowIp`,
`serverHost`, `idleSeconds`, `stallTimeout`, `clientOut`, `cutover`, `once`,
`noFirewall`, `streams`, `compress` (bool; `false` == `--no-compress`). **CLI wins over
config.** Do NOT auto-correct paths: a single backslash or a trailing comma is an
error to report, not silently fix (matches the existing STRICT-JSON stance).

### 7.5 Generated client - divergence (decide & document)
The PowerShell server writes a self-contained `.bat` that carries the PowerShell client
body after a `#FTPSBODY#` marker. A Rust client is a binary, so the equivalent is:

- Default: write a small connection file next to the binary, e.g.
  `download-scripts/ft-download-<name>.json` containing
  `{server, port, token, fingerprint, ignore, streams}`, AND print a ready-to-run
  command line: `ft get --server <h> --port <p> --token <t> --to <DEST> --fingerprint <fp> [...]`.
  `ft get --config <that.json> --to <DEST>` reads the file.
- Keep generating the PowerShell `.bat` too **only if** `ft-client.ps1` sits next to the
  binary (so a Windows receiver with only PowerShell still works). Optional, behind a
  flag; not required for v1.

Flag this section for the user before implementing - it is the one place with no exact
1:1 mapping.

## 8. Acceptance tests (interop matrix - definition of done)

Wire compatibility is proven by a cross-implementation matrix. For each cell, run a sync
and assert: identical file tree (path, bytes, size, mtime ticks), correct mirror deletes,
no spurious re-copies on a second run (everything "unchanged").

| sender \ receiver | PowerShell client | Rust client |
|---|---|---|
| PowerShell server | (today's baseline) | MUST pass |
| Rust server       | MUST pass         | MUST pass |

Each cell, exercise:
1. Single-stream (`SYNC`) and parallel (`QSYNC`, e.g. `--streams 4`).
2. Small-file bundles, large files raw, large files adaptive-compressed, incompressible
   large files (verify the skip), and a mixed corpus.
3. Resume/no-op: re-run -> 0 fetched, 0 deleted.
4. Mirror delete: remove a source file -> it disappears on the receiver; ignored files
   are never deleted.
5. Ignore patterns: name, path, `**`, dir-only - identical pruning on both sides.
6. Cutover (single-stream only): phase 1, signal, phase 2; a file deleted between phases
   is removed on the receiver.
7. Locked file on the source -> `-1` -> receiver keeps its copy, sync still completes.
8. Cross-OS: Rust server on Linux <-> Rust/PowerShell client on Windows and vice versa;
   verify mtime ticks survive the round trip.

Also keep the existing benchmark suite (`bench/`) meaningful: add a Rust-vs-PowerShell
column so we can show the speedup (small-file create floor is NTFS-bound and won't move
much; large-file and high-latency paths should improve).

## 9. Release packaging (both artifacts in one release)

Targets (static where possible):
- `x86_64-pc-windows-msvc` -> `ft-<ver>-x86_64-windows.zip`
- `x86_64-unknown-linux-musl` -> `ft-<ver>-x86_64-linux.tar.gz` (fully static)
- `aarch64-unknown-linux-musl` -> `ft-<ver>-aarch64-linux.tar.gz` (optional)
- `aarch64-apple-darwin` / `x86_64-apple-darwin` (optional)

Each archive contains the single `ft` binary (+ `LICENSE`, `README` excerpt). The
GitHub release for `v<ver>` carries the existing `folder-transfer-<ver>.zip`
(PowerShell) **and** all Rust archives. Extend `release.ps1` (or add a CI job) to build
the Rust targets and `gh release upload` them alongside the PowerShell ZIP - do not
remove or alter the PowerShell ZIP step.

## 10. Suggested milestones

1. **Skeleton + TLS handshake + pinning** (rustls + rcgen + fingerprint verifier);
   `AUTH`/`OK`, prove a Rust client authenticates to the PowerShell server and prints
   `FINGERPRINT=` parity.
2. **Single-stream client** against today's PowerShell server: `SYNC`, items (`D`/`B`/`F`),
   raw + adaptive decode, mtime ticks, safety-prefix, mirror delete. Pass matrix cells
   (PS server x Rust client, single-stream).
3. **Single-stream server**: `SYNC`, walk, bundles, large-file raw + adaptive encode
   (port section 6.6), cutover. Pass (Rust server x PS client).
4. **Parallel mode** both sides (`QSYNC`, producer/queue/handlers; client union + single
   mirror). Pass parallel cells.
5. **Config (JSONC), ignore parity, firewall (Win) / no-op (Linux), generated-client
   decision** (section 7.5).
6. **Cross-OS interop + benchmarks + release packaging** (section 9), then cut a release
   carrying both artifacts.

## 11. Reference: source of truth

The PowerShell scripts are normative. Key spots:
- Handshake / modes: `ft-server.ps1` (classic loop ~L825-885; parallel
  `Invoke-ParallelServe` ~L546-706) and `ft-client.ps1` (parallel ~L152-369;
  single-stream ~L371-561).
- Item stream: `Send-Bundle`/`Send-LargeFile`/`Send-Pass` in `ft-server.ps1`
  (~L324-544); client decoders in `ft-client.ps1` (bundle ~L418-462, large ~L464-531).
- Adaptive A/B: `Send-LargeFile` `Z` branch, `ft-server.ps1` ~L385-452.
- Ignore: `Test-IgnoredRel` + `Convert-GlobToRegex` (both files).
- mtime ticks: `LastWriteTimeUtc.Ticks` on send, `SetLastWriteTimeUtc` on receive.
- Token alphabet / cert subject / fingerprint: `ft-server.ps1` ~L732-761.
