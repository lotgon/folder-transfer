# folder-transfer

![license](https://img.shields.io/badge/license-MIT-blue)
![platform](https://img.shields.io/badge/platform-Windows-0078D6)
![powershell](https://img.shields.io/badge/PowerShell-5.1%2B-5391FE)
![install](https://img.shields.io/badge/install-none-brightgreen)

**Encrypted, zero‑install folder transfer & mirror‑sync between two Windows machines over TLS
— no service, no trace, with a two‑phase cutover mode for live databases.**

Pure PowerShell + the .NET that ships with Windows. You point it at one or more folders; it
serves them over TLS and shuts itself down, leaving the machine exactly as it was. Transfers are
**compressed on the fly only when it actually speeds things up** (adaptive — it measures the link
and stays raw on fast links or incompressible data), **many small files are bundled**, and
**several connections run in parallel** so transfers fly over high‑latency links. It generates
**one self‑contained file** to carry to the receiver.

> Status: young but functional; verified end‑to‑end on Windows 11 (loopback) and over a real
> two‑machine WAN link. Still review and test it in your environment before relying on it — see
> [Limitations](#limitations).

## Contents

- [Quick start](#quick-start)
- [Modes](#modes)
- [Many folders and ignoring](#many-folders-and-ignoring)
- [Parallel streams](#parallel-streams)
- [Performance](#performance)
- [Parameters](#parameters)
- [Progress and logs](#progress-and-logs)
- [Security](#security)
- [Firewall](#firewall)
- [Limitations](#limitations)
- [Troubleshooting](#troubleshooting)
- [Files](#files)

## Quick start

**Sender** — run with no arguments (or double‑click) and it asks what to share and which mode;
or pass the folder directly:

```bat
folder-transfer.bat D:\ProjectX            REM also: -AllowIp 10.0.0.7  -Once
```

It prints a fingerprint and writes a ready client to `download-scripts\ft-download-ProjectX.bat`.
Copy **that one file** to the receiver (it holds the token — treat it as a secret).

**Receiver** — run the generated file with a destination (or omit it and it asks; Enter = the
current folder):

```bat
ft-download-ProjectX.bat D:\incoming
```

The shared folder is recreated by name (`D:\incoming\ProjectX\…`). If the connection drops,
re‑run — it resumes. The window stays open at the end so you can read the result.

## Modes

Every transfer is a **mirror sync**: changed/new files are sent (by size + last‑write‑time),
files removed on the source are deleted on the receiver, unchanged files are skipped.

- **Single‑phase (default)** — one pass. Re‑run any time to catch up.
- **`-Cutover` (two‑phase, for a live database)** — pass 1 runs while the DB is up (no
  downtime); the server then **pauses** and prompts you to stop the DB and signal (press a key
  or create the printed `ft-cutover.go` file); pass 2 transfers only the delta. `-Cutover`
  implies `-Once`. Consistency depends on stopping the DB cleanly before pass 2.

## Many folders and ignoring

For a bigger job — several source folders and paths you want to skip (big log dirs, temp
files) — put everything in a **JSON config** and run `folder-transfer.bat sync.json` (a `.json`
first argument is auto‑detected as the config; `-Config sync.json` also works).
A ready‑to‑edit **`sync.example.json`** ships alongside the scripts — copy it and adjust:

```json
{
  "folders": ["C:/Users/YourName/Documents", "C:/Users/YourName/Pictures"],
  "ignore":  ["*.tmp", "~$*", "**/node_modules/", "**/cache/"],
  "once": true,
  "compress": true
}
```

(A migration example — copying your folders to a new PC. `*` matches within a name (`*.tmp`,
`~$*`); `**/node_modules/` and `**/cache/` skip those folders at **any** depth.)

- `folders` — each is shared and arrives under its own name (`<dest>\Bars\…`, `<dest>\Ticks\…`).
  It's standard JSON: in paths use **forward slashes** (`C:/path`) **or doubled backslashes**
  (`C:\\path`). A single backslash or a trailing comma is invalid and is reported (not auto‑fixed).
- `ignore` — patterns (see below); the rest of the keys are the same options as the command line
  (command‑line options override the JSON). `"compress": false` turns off compression.
- A ready `sync.example.json` with **every** key ships in the release — copy and trim it. The
  config accepts `//` and `/* */` **comments** (it's fully commented).
- You can also ignore from the command line: `-Ignore log/,*.tmp,mtlog`.

**Ignore pattern rules** (like `.gitignore`, case‑insensitive):

| Pattern | Matches |
|---------|---------|
| `log` | a file **or** folder named `log`, at any depth |
| `log/` | only a **folder** named `log` (a file named `log` is kept) — trailing `/` = directory‑only |
| `*.tmp` | anything ending in `.tmp` (`*` `?` are wildcards within a name) |
| `Bars/Reports/` | the `Reports` folder directly under the shared `Bars` (a pattern with `/` is a **path**, anchored at the shared‑folder name) |
| `*/cache/` | a `cache` folder exactly one level deep; `*` does not cross `/` |
| `**/cache/` | a `cache` folder at **any** depth (`**` spans `/`) |

A matched folder's **files** are skipped, but the folder (and its subfolders) is still
**recreated empty** on the receiver — some software won't start without them. Ignored content is
**never transferred and never deleted** on the receiver.

## Parallel streams

On a fast link with high latency (a long‑distance WAN, VPN, or anything with a big ping), a
**single** TCP connection can't fill the pipe — throughput is capped at roughly *window ÷
round‑trip‑time* no matter how much bandwidth you have. Opening several connections multiplies
that ceiling. folder-transfer uses **4 parallel streams by default** (`-Streams <n>`, or
`"streams": <n>` in JSON; `1` restores the classic single stream).

How it works: the sender lazily walks the source and assembles fine‑grained **work units** — a
bundle of small files, one large file, or an empty‑dir marker — into a shared queue; the N
connections pull units until it's drained. Because units are fine‑grained, even a **single huge
folder spreads across all streams** automatically — no need to split it in the config. Small files
are still bundled and data is still compressed (adaptively) within each stream.

Mirror stays exact. Every connection records what it received into one shared set; after **all**
streams finish cleanly, the receiver makes a single pass and deletes any local file the source no
longer has — including files under a whole top‑level folder you deleted. (Like single‑stream, it
deletes files, not directories, so an emptied folder may remain.) If any stream drops, the run is
considered incomplete and **nothing is deleted** that time — just re‑run.

## Performance

**Efficiency = goodput / channel capacity** — how much of the link the transfer actually uses.
Above 100% means adaptive compression delivered *more* original data than the wire physically
carried. Measured with the defaults (4 parallel streams + adaptive compression) over an emulated
WAN link, at 0 ms and at 150 ms round‑trip:

<!-- BENCH:TABLE START -->
| data type | 20 Mbit | 20 Mbit +150ms | 100 Mbit | 100 Mbit +150ms | 200 Mbit | 200 Mbit +150ms |
|---|---|---|---|---|---|---|
| small files (10000 x 4 KB) | 92% | 88% | 69% | 59% | 40% | 33% |
| large, incompressible (4 MB files, random) | 96% | 92% | 94% | 84% | 81% | 76% |
| large, compressible (4 MB files, text 3.73x) | 252% | 216% | 290% | 209% | 270% | 169% |
<!-- BENCH:TABLE END -->

- **Compressible** data goes *above 100%* — compression delivers several times the wire's raw rate.
- **Incompressible** data is link‑bound (~100%) — nothing to compress, so the link is saturated.
- **Small files** are bound by file creation on the receiver (NTFS metadata + antivirus), not the link.
- **High ping** (+150 ms) costs little; the gap only grows on the fastest links, where each transfer
  is so short that the few fixed round‑trips become a visible slice of it.

Absolute numbers are machine‑specific; efficiency (%) is largely transferable. Full report and the
test machine are in [BENCHMARKS.md](BENCHMARKS.md). Reproduce on your own hardware with
`powershell -ExecutionPolicy Bypass -File bench\bench.ps1` — method in [bench/README.md](bench/README.md).

## Parameters

Two separate programs: the **sender** (`folder-transfer.bat`) which you configure, and the
**receiver** (`ft-download-<name>.bat`) which the sender generates with everything baked in.
The token is auto‑generated and baked in — you never set it. Help: `folder-transfer.bat --help`.

**Sender** — give a folder (positional, first arg) *or* `-Config` with `folders`; names are
case‑insensitive:

| Option | Default | Meaning |
|--------|---------|---------|
| `<folder>` (positional) | required (or via `-Config`) | Folder to share, read‑only. |
| `<config.json>` / `-Config <file.json>` | — | JSON config (folders, ignore, options). A `.json` first argument is auto‑detected. |
| `-Ignore <list>` | none | Ignore name patterns, comma/semicolon separated (`log/,*.tmp`). |
| `-Streams <n>` | `4` | Parallel connections — big speed‑up on high‑latency links. `1` = classic single stream. See [Parallel streams](#parallel-streams). |
| `-Cutover` | off | Two‑phase sync for a live DB (implies `-Once`; forces `-Streams 1`). |
| `-AllowIp <ip>` | any | Serve only this client IP. |
| `-Once` | off | Close after one successful transfer. |
| `-IdleSeconds <n>` | `600` | Auto‑close after N s with no client connected. |
| `-StallTimeout <n>` | `300` | Abort a connected client silent for N s; keep listening. Raise (~1200) for big files over WAN. |
| `-Port <n>` | `8722` | TCP port. |
| `-ServerHost <addr>` | auto IPv4 | Address baked into the generated client. |
| `-ClientOut <path>` | `.\download-scripts\…` | Where to write the generated client. |
| `-NoFirewall` | off | Don't touch the firewall (opening it otherwise needs admin). |
| `-NoCompress` | off | Force compression **off**. Default is **adaptive**: the server compresses only when it measures it to be faster (see [Performance](#performance)). |
| `-Help` | — | Show help. |

**Receiver** — one optional argument:

| Argument | Default | Meaning |
|----------|---------|---------|
| `<destination_folder>` | prompted (Enter = current folder) | Where to sync into; the shared folder is recreated by name inside it. |

## Progress and logs

During a pass both sides print a throttled line (~every 2 s) with how many files are done,
fetched vs unchanged, data moved and speed — updated even mid‑file, so a large file never looks
frozen:

```
[serve …]   progress: 8120 files - sent 312, unchanged 7808, 1,604.0 MB @ 215.0 MB/s
[fetch] progress: 8120 files - fetched 312, unchanged 7808, 1,604.0 MB @ 215.0 MB/s
```

There is **no up‑front file count**, so the transfer starts immediately instead of pausing to
scan the whole tree first (the progress line therefore shows what's done so far, not "x of N" or
an ETA). The server also logs the client `IP:port`, each pass's file/byte counts, and how the
session ended.

## Security

| Layer | What it does |
|------|--------------|
| TLS 1.2 (`SslStream`) | Encrypts the whole session — vetted crypto, not hand‑rolled. |
| Certificate pinning | Client refuses any server whose cert fingerprint doesn't match (anti‑MITM). |
| Token (auto) | Random secret the client must present, sent inside TLS. |
| IP allow‑list | `-AllowIp` serves only one client IP. |
| Read‑only + path‑safe | The client requests files **by offset, never by path** — traversal and device names are impossible by construction. |
| No trace | Ephemeral cert and temporary firewall rule removed on exit; no service/user/config touched. |

It is still PowerShell: on a locked‑down host the real gates are PowerShell‑side (GPO
execution policy, WDAC/AppLocker Constrained Language Mode, EDR, admin for the firewall). The
sender stays thin (a fixed, readable `.ps1` you can allow‑list/sign). Full protocol and threat
model in [ARCHITECTURE.md](ARCHITECTURE.md).

## Firewall

Two independent gates: the **Windows Firewall** (the OS won't let packets reach the listener
until the port is open) and **`-AllowIp`** (the app only serves one source IP). folder-transfer
opens the port on start (needs admin; scoped to `-AllowIp` when set) and removes the rule on
exit. If the port is already open or managed elsewhere, pass `-NoFirewall`.

## Limitations

- Young tool: verified on Windows 11 (loopback) and over a real two‑machine WAN link, but test in
  your environment before trusting production data.
- The generated `.bat` holds the token in clear text — treat it as a secret and delete it after.
- Change detection is **size + mtime, not a hash**; a same‑size corruption isn't detected.
- A changed file is re‑fetched whole (no byte‑level resume within one huge file) — over a flaky
  WAN a very large file that drops near the end restarts from zero.
- A new self‑signed cert each run → fingerprint changes, so an old client bat won't connect to
  a new server instance (by design).
- **Exclusively‑locked** files are skipped for that pass (logged); files merely open for writing
  (e.g. DB logs) are read fine. Use `-Cutover` for a consistent live‑DB copy.
- **Parallel mode (`-Streams > 1`, the default)** deletes files, not directories, so a folder that
  becomes empty on the source may remain as an empty folder on the receiver (same as single‑stream).
  See [Parallel streams](#parallel-streams).
- Symlinks/junctions inside the shared folder are followed — don't share untrusted links.

## Troubleshooting

- **"Unknown Publisher" / "publisher could not be verified"** — Windows Mark‑of‑the‑Web on
  files from a downloaded ZIP. Fix: right‑click the **`.zip`** → Properties → **Unblock** →
  *then* extract (or `Get-ChildItem -Recurse | Unblock-File`). Only code‑signing removes it fully.
- **"running scripts is disabled"** — use the `.bat` wrappers; they call PowerShell with
  `-ExecutionPolicy Bypass`.
- **Client can't connect** — check the port is reachable and the baked `-ServerHost` is correct.
- **"remote certificate is invalid"** — the client bat is from a different server run; generate
  a fresh one.
- **Transfer interrupted** — just run the client bat again; it resumes.

## Files

Keep the three sender files together. `folder-transfer.bat` is a thin launcher for
`ft-server.ps1`, which embeds `ft-client.ps1` into the generated client.

| File | Side | Purpose |
|------|------|---------|
| `folder-transfer.bat` | sender | Thin launcher you run (asks interactively if given no args). |
| `ft-server.ps1` | sender | Server engine. |
| `ft-client.ps1` | sender | Client engine, embedded into the generated client. |
| `sync.example.json` | sender | Sample `-Config` file — copy, edit, and pass with `-Config`. |
| `download-scripts/ft-download-<name>.bat` | receiver | Generated single self‑contained file — the only thing you carry over. |

## License

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Andrei Pazniak. Provided as‑is, without
warranty; review the code and test in your environment before using it on production systems.
