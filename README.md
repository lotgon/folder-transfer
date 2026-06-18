# folder-transfer

![license](https://img.shields.io/badge/license-MIT-blue)
![platform](https://img.shields.io/badge/platform-Windows-0078D6)
![powershell](https://img.shields.io/badge/PowerShell-5.1%2B-5391FE)
![install](https://img.shields.io/badge/install-none-brightgreen)

**Transfer and mirror‑sync a whole folder from one Windows machine to another — encrypted,
zero‑install, one‑shot, and leaving no trace behind.**

folder-transfer is a tiny, self‑contained file server written in pure PowerShell (the .NET that
ships with Windows). You point it at a folder; it serves that folder once over TLS and
then shuts itself down. No Windows service, no OpenSSH, no SMB share, no extra user
account, no changes to the system config. When it exits, the machine is exactly as it
was before.

You run `folder-transfer.bat` on the sender (a thin launcher next to its readable `.ps1` scripts, so
the source box stays clean — no temp extraction, no obfuscation). It generates **one
self‑contained file** to carry to the receiver. Nothing is installed on either machine.

> Status: early but functional. Verified end‑to‑end on Windows 11 over loopback
> (transfer, integrity, TLS, certificate pinning, resume, path‑safety, auto‑shutdown,
> cleanup). A real two‑machine production run is recommended before you rely on it — see
> [Limitations](#limitations).

---

## Why use it

You occasionally need to move a folder (a few files, or 20 GB of them) between two
Windows servers, and the usual options are all heavy or risky on a locked‑down
production box:

- **SMB / file share** — needs shares, accounts, and stays open afterwards.
- **FTP / SFTP server** — installing OpenSSH Server (or similar) means a new service, a
  new account, global config changes, and something to uninstall and audit later.
- **Cloud / USB** — data leaves your perimeter or your hands.

folder-transfer is the in‑between: a **temporary, one‑shot** transfer that you start, use, and that
disappears by itself. Nothing to install, nothing to clean up, nothing left listening.

---

## Key features

- **Zero install** on both ends — uses only built‑in Windows PowerShell + .NET.
- **Encrypted** with TLS 1.2 (via .NET `SslStream` — vetted crypto, not hand‑rolled).
- **Anti‑spoofing** — the client pins the server's certificate fingerprint.
- **One‑file downloader for the receiver** — the server writes a single self‑contained
  `.bat` (connection details + the client embedded as plain text); copy just that one file.
- **Mirror sync** — the receiver becomes an exact copy of the source: changed/new files are
  transferred (by size + last‑write‑time), removed files are deleted. Re‑run any time to
  catch up; already‑synced files are skipped.
- **Two‑phase cutover** — for copying a live database with minimal downtime. See
  [the two modes](#the-two-modes).
- **Streaming, constant memory** — walks the tree lazily; works with millions of files.
- **Self‑closing** — exits after one transfer (`-Once`) or after an idle timeout.
- **No trace** — ephemeral certificate removed on exit; temporary firewall rule removed
  on exit; no service / user / global config touched.
- **Preserves the folder name** — `D:\ProjectX` arrives as `<dest>\ProjectX\...`.

---

## Security at a glance

| Layer | What it does |
|------|--------------|
| TLS 1.2 | Encrypts the whole session (auth token, file list, file bytes). |
| Certificate pinning | Client refuses any server whose cert fingerprint doesn't match — blocks MITM/spoofing. |
| Token (auto) | A random secret the client must present (auto‑generated, baked into the client, sent inside TLS). |
| IP allow‑list (optional) | Server serves only one client IP. |
| Read‑only + path‑safe | Served read‑only; the client requests files **by offset, never by path**, so directory traversal (`..\`) and Windows device names (`CON`, `PRN`, …) are impossible by construction. |
| Firewall (default on) | Opens the port only for the transfer (scoped to the allowed IP when set) and removes the rule on exit. |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the protocol, the full threat model, and the
design rationale.

---

## Requirements

- **Windows** with **PowerShell 5.1+** and **.NET Framework** — both ship with Windows
  10 / Windows Server 2016 and later. Nothing to install.
- The sender and receiver must have **network line‑of‑sight** on the chosen TCP port
  (default `8722`). On a locked‑down server the port is closed by default; folder-transfer opens it
  for you on start (this needs Administrator) and closes it on exit. See
  [Firewall vs. -AllowIp](#firewall-vs--allowip).

---

## Quick start

### On the SENDER (the machine that has the folder)

The simplest way is to just **run `folder-transfer.bat` with no arguments** (or
double‑click it): it asks what to share and in which mode (single‑phase or cutover),
the same way the receiver asks for its destination.

Or pass the folder directly — everything else has a sane default:

```bat
folder-transfer.bat D:\ProjectX
```

Or with the common options for a one‑shot, single‑client transfer:

```bat
folder-transfer.bat D:\ProjectX -AllowIp 10.0.0.7 -Once
```

- `D:\ProjectX` — the folder to share (required, just the path as the first argument).
- `-AllowIp 10.0.0.7` — only this client may connect (optional).
- `-Once` — shut down after one successful transfer.

The server prints its fingerprint and writes a ready‑to‑run client into
`download-scripts\`:

```
FINGERPRINT=8ad1bac7…3833
[serve] CLIENT WRITTEN -> …\download-scripts\ft-download-ProjectX.bat
       this is ONE self-contained file - copy just it to the receiver
```

### Move it to the receiver

Copy that **single** file, `ft-download-ProjectX.bat`, to the receiving machine over a
trusted channel — it is self‑contained (and contains the token, so treat it as a secret).

### On the RECEIVER

```bat
ft-download-ProjectX.bat D:\incoming
```

- The destination folder is **optional**. If you omit it (e.g. you just double‑click the
  file), it **asks** where to sync — press **Enter** to accept the default (the current
  folder).
- The shared folder is recreated **by name** inside it: you get `D:\incoming\ProjectX\…`.
- If the connection drops, just run it again — it **resumes**.
- The window **stays open** at the end (success or error) so you can read the result; press
  a key to close it.

That's it: the files land on the receiver, and the sender shuts itself down.

---

## Parameters

There are two separate programs with their own arguments: the **sender** (`folder-transfer.bat`, which
you configure) and the **receiver** (`ft-download-<name>.bat`, which the sender generates
with everything baked in). Don't mix them up.

### Sender — `folder-transfer.bat` (the server)

> Only the folder is required, and it's **positional** (first argument, no flag name).
> Everything else is optional. Parameter names are **case‑insensitive**. Help:
> `folder-transfer.bat --help`. **The two modes:** default = single‑phase sync; `-Cutover` =
> two‑phase sync.

| Option | Default | Meaning |
|--------|---------|---------|
| `<folder>` (positional) | — (required) | Folder to share, read‑only. |
| `-Cutover` | off | **Two‑phase sync:** pass 1 (DB live), you stop the DB, pass 2 (final). Implies `-Once`. |
| `-AllowIp <ip>` | any | Serve only this client IP. |
| `-Once` | off | Close right after **one** successful transfer (serve nobody else). |
| `-IdleSeconds <n>` | `600` | Auto‑close after N seconds with **no client connected**. |
| `-StallTimeout <n>` | `120` | Abort a connected client that sends nothing for N seconds, then keep listening. |
| `-Port <n>` | `8722` | TCP port. |
| `-ServerHost <addr>` | auto IPv4 | Address baked into the generated client. |
| `-ClientOut <path>` | `.\download-scripts\ft-download-<Folder>.bat` | Where to write the client (folder auto‑created; one per share accumulates). |
| `-NoFirewall` | off | Do **not** touch the firewall (by default the port is opened on start and closed on exit; opening needs Administrator). |
| `-Help` | — | Show help. |

> The **token** (client secret) is **auto‑generated** and baked into the generated client —
> you never set it. Every sync makes the destination an exact mirror of the source
> (changed/new files fetched, removed files deleted); there is no flag for it.

### Receiver — `ft-download-<name>.bat` (the client)

You don't configure the client — the sender bakes the server address, port, token and TLS
fingerprint into it. It takes a single, **optional** argument:

| Argument | Default | Meaning |
|----------|---------|---------|
| `<destination_folder>` (positional) | prompted (Enter = current folder) | Where to download into; the shared folder is recreated by name inside it (`<dest>\<FolderName>\…`). If omitted, the client asks for it interactively. |

```bat
ft-download-ProjectX.bat D:\incoming
```

---

## The two modes

Both modes are a **sync**: the receiver becomes an exact mirror of the source — changed
and new files are transferred (detected by **size + last‑write‑time**, unchanged files are
skipped without reading them), and files removed on the source are deleted on the receiver.

### Single‑phase sync (default)

```bat
folder-transfer.bat D:\data -ServerHost 10.0.0.5
```
One pass. Run it any time to bring the receiver up to date. Great for a folder you copy
once or re‑sync occasionally.

### Two‑phase sync — `-Cutover` (for a live database, minimal downtime)

> *Cutover* is the standard migration term for the final switch‑over: bulk‑copy while the
> source is live, then stop it and sync only the last delta.

```bat
folder-transfer.bat D:\db -ServerHost 10.0.0.5 -Cutover
```

You run the client **once**; it does **two passes in one session**, and the server drives
the cutover (`-Cutover` implies `-Once`, so it exits after pass 2; the idle timeout never
fires during the pause):

1. **Pass 1** runs while the database is still up — most of the data is copied, no downtime.
2. The server **pauses and prompts you** on its console: stop the database, then press any
   key (or create the printed `ft-cutover.go` file).
3. **Pass 2** runs against the now‑stopped, consistent files and transfers **only what
   changed** since pass 1 (and deletes anything removed in between) — a small, fast delta,
   in the same client run. This is the only downtime. The server then shuts down.

If the connection drops during the pause, just re‑run the client after the database is
stopped — a single sync pass against the stopped files is itself the final delta.

> **Consistency:** a file copy of a *running* database is crash‑consistent at best. The
> final copy is consistent if the database was stopped cleanly before pass 2. For
> zero‑downtime consistent snapshots, the proper tool is the database's own online backup
> or a VSS snapshot — see [ARCHITECTURE.md](ARCHITECTURE.md). Delta detection here is
> size+mtime, not a content hash (see [Limitations](#limitations)).

---

## Firewall vs. -AllowIp

These are two different layers and both matter:

- **Windows Firewall is the OS gate.** On a server, inbound is denied by default. Until
  the port is open, the client's packets never even reach folder-transfer — so you must open it for
  the legitimate receiver to connect at all. folder-transfer does this by default and removes the
  rule on exit. When `-AllowIp` is set, the firewall rule is scoped to that one IP.
- **`-AllowIp` is the application gate.** After a connection arrives, the server checks
  the source IP and rejects anyone else.

`-AllowIp` alone does nothing if the firewall blocks the port (the connection never
arrives); an open port alone lets anyone reachable hit the listener (still gated by the
token and the pinned certificate). Using both is defense in depth.

If the port is already open (or the firewall is managed elsewhere), pass `-NoFirewall`.

---

## Server console & timeouts

The server logs every step to its console with timestamps — including the client's
`IP:port`, each transfer pass with file/byte counts, and how each session ended:

```
[serve 14:02:31] session #1 connected from 10.0.0.7:60434 (TLS ok; stall timeout 120s)
[serve 14:02:31] session #1: sync pass 1 - scanning D:\db
[serve 14:02:34] session #1: pass 1 done - changed/new 812, unchanged 19034, 4,210,118,400 bytes
[serve 14:02:34] session #1: cutover - WAITING for you to stop the database and signal ...
[serve 14:09:10] session #1: pass 2 done - changed/new 11, unchanged 19835, 26,214,400 bytes
[serve 14:09:10] session #1 completed cleanly in 399s
```

Two independent timeouts govern when the server gives up:

- **`-IdleSeconds`** (default 600) — counts only **between** connections. After a session
  ends in a non‑`-Once` server, it waits this long for the next client and then auto‑closes.
  It does **not** run while a client is connected.
- **`-StallTimeout`** (default 120) — if a **connected** client sends nothing for this long,
  that session is aborted with an error and the server keeps listening (the client can
  reconnect). This stops a hung client from blocking the server forever.

A dropped or aborted session never counts as completion, so `-Once` does not exit on it.

---

## Security & antivirus

- **Plain text, no obfuscation** everywhere — readable batch/PowerShell, **no base64, no
  compiled binary**.
- **Sender stays thin.** On the source box (often the sensitive one), `folder-transfer.bat` is a
  one‑line wrapper that runs the fixed, readable `ft-server.ps1` — **no temp extraction**,
  so the scripts can be **AppLocker/WDAC allow‑listed and code‑signed**. (You can also just
  run `ft-server.ps1` directly.)
- **The generated receiver file is one self‑contained `.bat`.** For convenience it embeds
  the client and, when run, writes a temp `.ps1` and executes it. That "script writes+runs
  a temp script" pattern can be flagged by aggressive EDR/AppLocker **on the receiver**. If
  the receiver is also hardened, run `ft-client.ps1` directly there instead (pass
  `-Server/-Port/-Token/-ToFolder/-Fingerprint` — the values are visible at the top of the
  generated `.bat`).
- **It is still PowerShell.** A `.bat` does not bypass PowerShell's controls, so on a
  locked‑down server the real gates are PowerShell‑side and apply equally:
  - **Execution policy via GPO** (`AllSigned` / `Restricted`) overrides `-ExecutionPolicy
    Bypass` — scripts won't run unless allowed or signed.
  - **WDAC / AppLocker Constrained Language Mode** blocks the .NET types folder-transfer needs
    (`TcpListener`, `SslStream`). If CLM is enforced, folder-transfer cannot run as a script — you'd
    need the scripts allow‑listed/signed so they run in Full Language.
  - **EDR** may still alert on a process opening a listening socket and reading many files.
  - **Admin** is needed for the default firewall opening.
  - Quick check on the host: `$ExecutionContext.SessionState.LanguageMode` (want
    `FullLanguage`) and `Get-ExecutionPolicy -List`.
- **Mark‑of‑the‑Web / "Unknown Publisher" dialog:** files extracted from a ZIP you downloaded
  (e.g. a GitHub release) inherit the internet "blocked" mark, so Windows shows *“Open File –
  Security Warning … The publisher could not be verified … Unknown Publisher”* the first time
  you run `folder-transfer.bat`. This is expected for any unsigned downloaded script — it is
  not specific to this tool. To clear it:
  - **Best:** right‑click the **`.zip`** → *Properties* → tick **Unblock** → *OK*, **then**
    extract. The mark is removed from every file at once.
  - Already extracted? Run in the folder:
    `Get-ChildItem -Recurse | Unblock-File` (or right‑click each file → *Unblock*).
  - The dialog only disappears entirely if the scripts are **code‑signed** (needs a certificate).
- `-ExecutionPolicy Bypass` is passed only to the launched PowerShell process; it does not
  change the machine's execution policy.

---

## Limitations

Read these before trusting it with production data:

- **Tested on Windows 11 over loopback only.** The logic is standard, but do one real
  two‑machine run first — especially the firewall auto‑open, which needs Administrator.
- **The generated `.bat` contains the token in clear text.** Treat it as a secret;
  delete it after use.
- **Resume/skip is by file size, not by hash.** A corruption that keeps the same size
  would not be detected (an optional integrity check is on the roadmap).
- **Token attempts are not rate‑limited** (mitigated by the short‑lived server,
  non‑standard port, optional `-AllowIp`, and TLS pinning).
- **Self‑signed cert is regenerated every run**, so the fingerprint changes — an old
  client bat won't connect to a new server instance (by design).
- **Symlinks/junctions inside the shared folder** are followed by enumeration and could
  point outside the folder; don't share a folder containing untrusted links.
- **Exclusively‑locked files** (opened by another process with no sharing) can't be read and
  are skipped for that pass with a log line — the sync still finishes. Files merely open for
  writing (e.g. database logs) are read fine. For a consistent copy of a live database use
  `-Cutover`: pass 2 runs after you stop it.
- After a **hard kill** (not a normal exit) a firewall rule may linger — harmless
  (nothing is listening) and auto‑cleaned on the next start.

---

## Troubleshooting

- **"running scripts is disabled" / execution policy** — use the `.bat` wrappers; they
  already call PowerShell with `-ExecutionPolicy Bypass`.
- **Client can't connect** — check the port is reachable from the receiver to the sender,
  and that the baked `-ServerHost` address is correct.
- **"remote certificate is invalid"** — the fingerprint didn't match: the client bat is
  from a different server run. Generate and use a fresh one.
- **Transfer interrupted** — just run the client bat again; it resumes.

---

## Files

Keep the three sender files together in one folder. `folder-transfer.bat` is a **thin launcher** that
runs `ft-server.ps1` (which reads `ft-client.ps1` to bake into the generated client).

| File | Side | Purpose |
|------|------|---------|
| **`folder-transfer.bat`** | sender | Thin launcher you run (`powershell -File ft-server.ps1 %*`). |
| `ft-server.ps1` | sender | Server engine (the real, readable script). |
| `ft-client.ps1` | sender | Client engine; embedded (plain text) into the generated client. |
| `download-scripts/ft-download-<name>.bat` | receiver | **Generated single self‑contained file** — copy just this one to the receiver. |
| `README.md` / `ARCHITECTURE.md` / `CHANGELOG.md` | — | Docs. |

> The **sender** stays thin (the `.bat` launches the adjacent `.ps1`, no temp extraction).
> The **generated receiver file** is one self‑contained `.bat` (plain‑text client embedded;
> it writes a temp `.ps1` to run on the receiver).

---

## License

Released under the **MIT License** — see [LICENSE](LICENSE). Copyright (c) 2026 Andrei Pazniak.

## Disclaimer

folder-transfer is provided as‑is, without warranty. It moves data over the network and opens a
firewall port; review the code and test it in your environment before using it on
production systems.
