# folder-transfer

![license](https://img.shields.io/badge/license-MIT-blue)
![platform](https://img.shields.io/badge/platform-Windows-0078D6)
![powershell](https://img.shields.io/badge/PowerShell-5.1%2B-5391FE)
![install](https://img.shields.io/badge/install-none-brightgreen)

**Encrypted, zero‑install folder transfer & mirror‑sync between two Windows machines over TLS
— no service, no trace, with a two‑phase cutover mode for live databases.**

Pure PowerShell + the .NET that ships with Windows. You point it at a folder; it serves that
folder over TLS and shuts itself down, leaving the machine exactly as it was. Transfers are
**compressed on the fly** (smartly skipping already‑compressed files). It generates **one
self‑contained file** to carry to the receiver.

> Status: early but functional; verified end‑to‑end on Windows 11. Do one real two‑machine
> run before relying on it — see [Limitations](#limitations).

## Contents

- [Quick start](#quick-start)
- [Modes](#modes)
- [Many folders and ignoring](#many-folders-and-ignoring)
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
files) — put everything in a **JSON config** and run `folder-transfer.bat -Config sync.json`.
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

A matched folder is skipped whole. Ignored content is **never transferred and never deleted**
on the receiver.

## Parameters

Two separate programs: the **sender** (`folder-transfer.bat`) which you configure, and the
**receiver** (`ft-download-<name>.bat`) which the sender generates with everything baked in.
The token is auto‑generated and baked in — you never set it. Help: `folder-transfer.bat --help`.

**Sender** — give a folder (positional, first arg) *or* `-Config` with `folders`; names are
case‑insensitive:

| Option | Default | Meaning |
|--------|---------|---------|
| `<folder>` (positional) | required (or via `-Config`) | Folder to share, read‑only. |
| `-Config <file.json>` | — | JSON with `folders`, `ignore`, and any options below. |
| `-Ignore <list>` | none | Ignore name patterns, comma/semicolon separated (`log/,*.tmp`). |
| `-Cutover` | off | Two‑phase sync for a live DB (implies `-Once`). |
| `-AllowIp <ip>` | any | Serve only this client IP. |
| `-Once` | off | Close after one successful transfer. |
| `-IdleSeconds <n>` | `600` | Auto‑close after N s with no client connected. |
| `-StallTimeout <n>` | `300` | Abort a connected client silent for N s; keep listening. Raise (~1200) for big files over WAN. |
| `-Port <n>` | `8722` | TCP port. |
| `-ServerHost <addr>` | auto IPv4 | Address baked into the generated client. |
| `-ClientOut <path>` | `.\download-scripts\…` | Where to write the generated client. |
| `-NoFirewall` | off | Don't touch the firewall (opening it otherwise needs admin). |
| `-NoCompress` | off | Disable streaming compression (it is **on by default**). |
| `-Help` | — | Show help. |

**Receiver** — one optional argument:

| Argument | Default | Meaning |
|----------|---------|---------|
| `<destination_folder>` | prompted (Enter = current folder) | Where to sync into; the shared folder is recreated by name inside it. |

## Progress and logs

During a pass both sides print a throttled line (~every 2 s) with files done / left, fetched
vs unchanged, data moved, speed and an ETA:

```
[serve …]   progress: 8120/19846 files (11726 left) - sent 312, unchanged 7808, 1,604.0 MB @ 215.0 MB/s, ETA 00:00:18
[fetch] progress: 8120/19846 (11726 left) - fetched 312, unchanged 7808, 1,604.0 MB @ 215.0 MB/s, ETA 00:00:18
```

The server also logs the client `IP:port`, each pass's file/byte counts, and how the session
ended. Speed is the last‑interval rate; ETA is estimated from the file rate (a guide, not a
guarantee).

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

- Verified on Windows 11 over loopback; do one real two‑machine run first.
- The generated `.bat` holds the token in clear text — treat it as a secret and delete it after.
- Change detection is **size + mtime, not a hash**; a same‑size corruption isn't detected.
- A changed file is re‑fetched whole (no byte‑level resume within one huge file).
- A new self‑signed cert each run → fingerprint changes, so an old client bat won't connect to
  a new server instance (by design).
- **Exclusively‑locked** files are skipped for that pass (logged); files merely open for writing
  (e.g. DB logs) are read fine. Use `-Cutover` for a consistent live‑DB copy.
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
