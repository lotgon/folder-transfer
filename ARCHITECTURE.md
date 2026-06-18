# folder-transfer — Architecture & Security

This document explains how folder-transfer works under the hood, why it is safe to use, and the
design decisions behind it. For day‑to‑day usage see [README.md](README.md).

---

## 1. Components

```
SENDER (has the folder)                 RECEIVER (wants the folder)
┌─────────────────────────┐             ┌────────────────────────────┐
│ folder-transfer.bat   (thin)       │  ─ TLS/TCP ▶ │ ft-download-<name>.bat   │
│ ft-server.ps1 (server) │             │   (one self-contained file)│
│ ft-client.ps1 (client) │             └────────────────────────────┘
└─────────────────────────┘
        │ generates  ▼
 download-scripts\ft-download-<name>.bat  ── ONE file copied to the receiver ──▶
```

Asymmetric on purpose: the **sender stays thin** (a fixed, readable `.ps1` the source box
can allow‑list/sign — no temp extraction), while the **one file that travels** to the
receiver is self‑contained for convenience.

- **`folder-transfer.bat`** — a one‑line launcher: `powershell -ExecutionPolicy Bypass -File
  ft-server.ps1 %*` (plus `--help` routing). The real logic is in the adjacent `.ps1`.
- **`ft-server.ps1`** — the server. Opens a TCP listener, wraps each connection in TLS,
  serves the chosen folder read‑only, then shuts down. To build a client it reads
  `ft-client.ps1` and embeds it (plain text) into the generated `.bat`.
- **`ft-client.ps1`** — the client engine, embedded into the generated client.
- **`ft-download-<name>.bat`** — the **single self‑contained file** the receiver runs:
  a batch header (baked connection details) plus the plain‑text client after a marker; the
  header writes the client to a temp `.ps1`, runs it, and deletes it. No base64.

Both sides rely solely on the PowerShell + .NET that ship with Windows.

---

## 2. The wire protocol

The transport is **TCP**, wrapped in **TLS 1.2** (.NET `SslStream`). On top of TLS sits a
deliberately tiny, custom, request/response protocol. Control messages are UTF‑8 lines
terminated by `\n`; file bodies are raw bytes. There is one operation — a **sync**, made of
one or more **passes** (two for cutover):

```
client → AUTH <token>
server → OK                            (or "ERR auth" and disconnect)
client → SYNC
server →   F <size> <mtime> <rel> …    ─┐ one pass: every file is offered, streamed
client →   0 (fetch) | -1 (skip)        │ lazily as the server walks the tree
server →   <bytes-remaining> + raw …   ─┘ (only for files the client wants)
server → PASS-END
            ── if -Cutover: server sends PING keepalives while it waits ──
server → GO                            (operator stopped the DB and signalled; 2nd pass)
server →   F … PASS-END                 …
server → DONE
client → BYE
```

Key points:

- **Control lines are read one byte at a time up to `\n`**, so the reader never buffers
  ahead and corrupts the binary file body that follows a `<bytes-remaining>` line.
- **Lazy, server‑driven walk.** The server walks the tree with an explicit stack and
  `[IO.Directory]::EnumerateFiles/EnumerateDirectories` (lazy enumerators), emitting one
  file at a time as `F <size> <mtime-ticks> <rel>`. Nothing is enumerated up front, so
  memory stays constant even for millions of files.
- **Change detection** is by **size + last‑write‑time (UTC ticks)**, no content hashing.
  For each offered file the client compares its local copy: missing / different size /
  different mtime → fetch in full (overwrite, offset `0`); identical → skip (`-1`). After
  writing, the client **sets the file's mtime to the source's** so the next sync compares
  correctly. (Because a changed file is overwritten whole, there is no byte‑level resume
  within a single huge file — an interrupted file is re‑fetched next run; block‑level delta
  is future work.)
- **Mirror is intrinsic.** The client records the paths offered in the **latest pass** only
  (the set resets at the start of each pass). After a clean `DONE` it deletes any local file
  under `<dest>\<FolderName>` not in that final set — so a file removed on the source (incl.
  **between cutover pass 1 and pass 2**) is deleted on the client. Deletion is scoped to that
  subtree (derived from the offered paths, never the whole destination) and is **skipped if
  the sync did not finish cleanly**, so a dropped connection never deletes wrongly.
- **Two‑phase cutover (`-Cutover`):** two passes in **one session** (implies `-Once`, so the
  server exits after pass 2). Pass 1 runs while the database is live (most data, no
  downtime). The server then waits for the operator (keypress at its console, or an
  `ft-cutover.go` flag file — works headless), sending `PING` keepalives so an idle
  NAT/firewall doesn't drop the connection. The idle timeout does **not** apply during this
  pause. After the DB is stopped the operator signals; pass 2 transfers only the delta
  against the now‑consistent files. If the held connection drops during the pause, just
  re‑run the client after the DB is stopped — a single sync pass *is* the final delta.
- **`-Once` exits only on a *clean* completion** (the client's `BYE`). A dropped connection
  is not a completion, so the server keeps listening and the client can reconnect and finish.
- **Folder name preservation:** the server makes paths relative to the *parent* of the
  shared folder, so the folder's own name becomes the top‑level directory on the receiver
  (`<dest>\<FolderName>\…`).

---

## 3. Security model

Defense in depth, from the outside in. A request must pass every layer.

### 3.1 Network reachability — Windows Firewall (OS gate)
On a hardened server, inbound is denied by default, so nobody — not even the legitimate
receiver — can reach the listener until the port is opened. folder-transfer opens its port on start
(needs Administrator) and **removes the rule on exit**. When `-AllowIp` is set, the rule
is scoped to that single source IP (`-RemoteAddress`). This is the OS‑level gate.

### 3.2 Source IP — `-AllowIp` (application gate)
After a connection arrives, the server checks the remote IP and drops anything that isn't
the allowed address. This is independent of the firewall: it still applies if the
firewall is managed externally (`-NoFirewall`), and it backs up the firewall rule if that
rule is ever changed or removed.

> Firewall and `-AllowIp` are **not** redundant. The firewall decides *whether packets
> reach the program at all*; `-AllowIp` decides *whom the program serves once they do*.
> `-AllowIp` alone is useless behind a closed port; an open port alone still leaves token
> + certificate pinning in the way.

### 3.3 Channel encryption — TLS 1.2
Every byte after the TCP handshake is inside a TLS session. The crypto is .NET's
`SslStream` (the Windows SChannel implementation) — **vetted, not hand‑rolled**. folder-transfer's
own code never implements ciphers, key exchange, or MACs.

### 3.4 Server authenticity — certificate pinning (anti‑MITM)
The server mints a short‑lived self‑signed certificate at startup (RSA‑2048, in the
current user's certificate store, removed on exit) and prints its **SHA‑256 fingerprint**.
That fingerprint is baked into the generated client bat. The client's certificate
validation callback accepts the connection **only if the presented certificate's SHA‑256
matches the pinned value** — so a man‑in‑the‑middle or a spoofed server (which cannot
present the matching private key) is refused with "remote certificate is invalid".

### 3.5 Client authentication — token
A shared secret (the **token**) is sent by the client as the first message *inside TLS*; a
mismatch rejects the session. The token is **auto‑generated** (random, per server run) and
baked into the generated client — there is no manual option, so client auth is always on.
The token is the only secret embedded in the generated bat — treat that file accordingly.

### 3.6 Scope & path safety — read‑only, by construction
The folder is served **read‑only**. Crucially, **the client never sends a file path** —
it only sends an integer offset in response to each file the server *offers*. The server
opens exactly the files it discovered itself while enumerating inside the shared folder.

This eliminates an entire class of bugs by construction, rather than by sanitising
untrusted input:

- **Directory traversal** (`..\..\windows\…`) — impossible: there is no client‑supplied
  path to traverse.
- **Windows reserved device names** (`CON`, `PRN`, `AUX`, `NUL`, `COM1…9`, `LPT1…9`, and
  variants like `CON.txt`) — impossible: device names are not real directory entries, so
  they never appear in the server's enumeration, and the client cannot name them.
- **ADS streams, UNC paths, trailing dots/spaces, 8.3 short names** — same reasoning.

The client additionally verifies that each save path stays within the destination folder,
so a hostile server cannot make the client write outside `<destination>`.

> Earlier prototypes accepted a client‑supplied relative path in `GET` and joined it onto
> the shared folder — the classic "trust the input" mistake that allowed `..\` traversal.
> The current offset‑based protocol removes the untrusted path entirely. Hand‑rolling a
> Windows path sanitiser (device names, ADS, short names, …) is a losing game; not
> parsing untrusted paths at all is the correct fix.

### 3.7 Footprint & cleanup
On a normal exit (`-Once`, idle timeout, or error) the server, in a `finally` block:
removes its certificate from the store and removes its firewall rule. There is **no
service, no user account, no global config change** at any point. After exit the machine
is back to its original state.

---

## 4. Threat model & limitations

What folder-transfer defends against, and what it does not:

**Defended:**
- Passive eavesdropping → TLS.
- Server spoofing / MITM → certificate pinning.
- Unauthorised clients reaching an open port → token + `-AllowIp`.
- Path‑based attacks (traversal, device names) → offset‑only protocol.
- Lingering exposure → one‑shot / idle auto‑shutdown + cleanup.

**Not defended / known gaps:**
- **The generated bat holds the token in clear text.** Protect and delete it.
- **No rate limiting** on token guesses (mitigated by short lifetime, non‑standard port,
  IP allow‑list, pinning).
- **Integrity is size‑based**, not hash‑based; equal‑size corruption isn't detected.
- **Symlinks/junctions** inside the shared folder are followed and could escape it.
- **Token is compared non‑constant‑time** (negligible over TLS on a short‑lived server,
  but worth noting).
- **Verified on loopback only** so far; validate a real cross‑machine run.

---

## 5. Design decisions

- **Why a custom PowerShell program instead of SFTP/HTTPS?** To avoid installing or
  altering anything on a sensitive production box. A real SFTP/HTTPS server means a new
  service, account, and config to manage and later remove. folder-transfer runs as a plain,
  short‑lived process and removes its own traces. .NET (built into Windows) supplies TLS,
  so there are no third‑party dependencies. The trade‑off — a small custom application
  protocol instead of a battle‑tested one — is kept minimal: the custom layer is just
  framing, and all crypto and file I/O are delegated to vetted libraries.
- **Why a streaming walk instead of sending a file list?** A folder can hold millions of
  files; building and sending a full manifest first would cost memory and a slow start.
  Lazy enumeration starts transferring immediately at constant memory.
- **Why offset‑based requests instead of paths?** Security (section 3.6) and simplicity:
  the client only ever sends integers, so there is nothing to sanitise.
- **Why is sync always a mirror (no flag)?** "Sync" means the destination should match the
  source; a toggle to *not* delete removed files just adds confusion. Deletion is scoped to
  the copied subtree and only runs on a clean finish, so it is safe.

---

## 6. File map

| File | Role |
|------|------|
| `folder-transfer.bat` | Thin launcher: `powershell -File ft-server.ps1 %*` (+ `--help` routing). |
| `ft-server.ps1` | Server engine: TLS, firewall, client generation, the `SYNC` server (one/two passes), logging. |
| `ft-client.ps1` | Client engine: TLS + pinning, the `SYNC` client, change detection, mirror‑delete, save‑path guard. |
| `ft-download-<name>.bat` | Generated **single self‑contained** downloader: baked connection details + plain‑text client body; the only file that travels to the receiver. |
