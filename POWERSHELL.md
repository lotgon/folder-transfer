# folder-transfer — PowerShell edition (Windows-only)

> This is the original **pure-PowerShell** edition (Windows only). The project's primary tool is now
> the cross-platform Rust binary **`ft`** — see the [main README](README.md). This page is kept for
> people who specifically want the zero-binary, script-only variant on Windows.

**Encrypted, zero‑install folder transfer & mirror‑sync between two Windows machines over TLS
— no service, no trace, with a two‑phase cutover mode for live databases.** Pure PowerShell + the
.NET that ships with Windows.

You point it at one or more folders; it serves them over TLS and shuts itself down, leaving the
machine exactly as it was. Transfers are **compressed on the fly only when it actually speeds things
up** (adaptive), **many small files are bundled**, and **several connections run in parallel**. It
generates **one self‑contained `.bat`** to carry to the receiver.

## Quick start

**Sender** — run with no arguments (or double‑click) and it asks what to share and which mode; or
pass the folder directly:

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
re‑run — it resumes.

## Modes

Every transfer is a **mirror sync**: changed/new files are sent (by size + last‑write‑time), files
removed on the source are deleted on the receiver, unchanged files are skipped.

- **Single‑phase (default)** — one pass. Re‑run any time to catch up.
- **`-Cutover` (two‑phase, for a live database)** — pass 1 runs while the DB is up; the server then
  **pauses** and prompts you to stop the DB and signal (press a key or create the printed
  `ft-cutover.go` file); pass 2 transfers only the delta. `-Cutover` implies `-Once`.

## Many folders and ignoring

Put several source folders and ignore patterns in a **JSON config** and run
`folder-transfer.bat sync.json` (a `.json` first argument is auto‑detected; `-Config sync.json`
also works). A ready `sync.example.json` ships alongside the scripts.

```json
{
  "folders": ["C:/Users/YourName/Documents", "C:/Users/YourName/Pictures"],
  "ignore":  ["*.tmp", "~$*", "**/node_modules/", "**/cache/"],
  "once": true,
  "compress": true
}
```

Paths use **forward slashes** (`C:/path`) **or doubled backslashes** (`C:\\path`). `//` and `/* */`
comments are allowed. Command‑line options override the JSON.

**Ignore pattern rules** (like `.gitignore`, case‑insensitive):

| Pattern | Matches |
|---------|---------|
| `log` | a file **or** folder named `log`, at any depth |
| `log/` | only a **folder** named `log` — trailing `/` = directory‑only |
| `*.tmp` | anything ending in `.tmp` (`*` `?` are wildcards within a name) |
| `Bars/Reports/` | the `Reports` folder directly under the shared `Bars` (a `/` pattern is a path, anchored at the shared‑folder name) |
| `*/cache/` | a `cache` folder exactly one level deep; `*` does not cross `/` |
| `**/cache/` | a `cache` folder at **any** depth (`**` spans `/`) |

A matched folder's **files** are skipped, but the folder is still **recreated empty** on the
receiver. Ignored content is **never transferred and never deleted**.

## Parameters (sender)

| Option | Default | Meaning |
|--------|---------|---------|
| `<folder>` (positional) | required (or via `-Config`) | Folder to share, read‑only. |
| `<config.json>` / `-Config <file.json>` | — | JSON config. A `.json` first argument is auto‑detected. |
| `-Ignore <list>` | none | Ignore patterns, comma/semicolon separated. |
| `-Streams <n>` | `4` | Parallel connections; `1` = classic single stream. |
| `-Cutover` | off | Two‑phase sync for a live DB (implies `-Once`; forces `-Streams 1`). |
| `-AllowIp <ip>` | any | Serve only this client IP. |
| `-Once` | off | Close after one successful transfer. |
| `-IdleSeconds <n>` | `600` | Auto‑close after N s with no client. |
| `-StallTimeout <n>` | `300` | Abort a client silent for N s; keep listening. |
| `-Port <n>` | `8722` | TCP port. |
| `-ServerHost <addr>` | auto IPv4 | Address baked into the generated client. |
| `-ClientOut <path>` | `.\download-scripts\…` | Where to write the generated client. |
| `-NoFirewall` | off | Don't touch the firewall. |
| `-NoCompress` | off | Force compression off (default is adaptive). |
| `-Help` | — | Show help. |

**Receiver** — one optional argument: `<destination_folder>` (prompted if omitted; Enter = current).

## Security

| Layer | What it does |
|------|--------------|
| TLS 1.2 (`SslStream`) | Encrypts the whole session. |
| Certificate pinning | Client refuses any server whose cert fingerprint doesn't match (anti‑MITM). |
| Token (auto) | Random secret the client must present, sent inside TLS. |
| IP allow‑list | `-AllowIp` serves only one client IP. |
| Read‑only + path‑safe | The client requests files **by offset, never by path** — traversal is impossible by construction. |
| No trace | Ephemeral cert and temporary firewall rule removed on exit. |

Full protocol and threat model: [ARCHITECTURE.md](ARCHITECTURE.md). Benchmarks for this edition:
[BENCHMARKS.md](BENCHMARKS.md) (reproduce with `powershell -ExecutionPolicy Bypass -File bench\bench.ps1`).

## Troubleshooting

- **"Unknown Publisher"** — Mark‑of‑the‑Web: right‑click the **`.zip`** → Properties → **Unblock** → then extract.
- **"running scripts is disabled"** — use the `.bat` wrappers (they call PowerShell with `-ExecutionPolicy Bypass`).
- **"remote certificate is invalid"** — the client bat is from a different server run; generate a fresh one.
- **Transfer interrupted** — run the client bat again; it resumes.

## Files

| File | Side | Purpose |
|------|------|---------|
| `folder-transfer.bat` | sender | Thin launcher you run. |
| `ft-server.ps1` | sender | Server engine. |
| `ft-client.ps1` | sender | Client engine, embedded into the generated client. |
| `sync.example.json` | sender | Sample `-Config` file. |
| `download-scripts/ft-download-<name>.bat` | receiver | Generated self‑contained file. |

## Limitations

- Change detection is **size + mtime**, not a hash.
- A changed file is re‑fetched whole (no byte‑level resume within one huge file).
- A new self‑signed cert each run → an old client bat won't connect to a new server instance.
- Exclusively‑locked files are skipped for that pass; use `-Cutover` for a consistent live‑DB copy.
- Symlinks/junctions inside the shared folder are followed — don't share untrusted links.

## License

MIT — see [LICENSE](LICENSE).
