# folder-transfer — Architecture & Security

How folder-transfer works under the hood, why it is safe to use, and the design decisions
behind it. For day‑to‑day usage see [README.md](README.md).

---

## 1. Components

```
SENDER (has the folders)                  RECEIVER (wants them)
┌──────────────────────────┐              ┌────────────────────────────┐
│ folder-transfer.bat (thin)│  ─ TLS/TCP ▶ │ ft-download-<name>.bat      │
│ ft-server.ps1  (server)   │              │   (one self-contained file)│
│ ft-client.ps1  (client)   │              └────────────────────────────┘
│ sync.example.json         │
└──────────────────────────┘
        │ generates  ▼
 download-scripts\ft-download-<name>.bat  ── ONE file copied to the receiver ──▶
```

Asymmetric on purpose: the **sender stays thin** (fixed, readable `.ps1` files the source box
can allow‑list/sign — no temp extraction), while the **one file that travels** to the receiver
is self‑contained for convenience.

- **`folder-transfer.bat`** — a one‑line launcher: `powershell -ExecutionPolicy Bypass -File
  ft-server.ps1 %*` (plus `--help` routing, a final pause, and no‑args → interactive prompts).
- **`ft-server.ps1`** — the server. Reads config (CLI and/or JSON), opens a TCP listener, wraps
  each connection in TLS, serves the chosen folder(s) read‑only, then shuts down. To build a
  client it embeds `ft-client.ps1` (plain text) into the generated `.bat`.
- **`ft-client.ps1`** — the client engine, embedded into the generated client.
- **`ft-download-<name>.bat`** — the **single self‑contained file** the receiver runs: a batch
  header (baked connection details) plus the plain‑text client after a `#FTPSBODY#` marker; the
  header writes the client to a temp `.ps1`, runs it, deletes it. No base64.
- **`sync.example.json`** — a fully commented sample `-Config` file.

Both sides rely solely on the PowerShell + .NET that ship with Windows.

---

## 2. The wire protocol

Transport is **TCP** with `TCP_NODELAY` (Nagle off — important for many small files over WAN),
wrapped in **TLS 1.2** (.NET `SslStream`). On top sits a tiny custom protocol: control messages
are UTF‑8 lines terminated by `\n`; file bodies are raw bytes. Control lines are read **one byte
at a time up to `\n`**, so the reader never buffers ahead and corrupts the binary body that
follows a length line.

One operation — a **sync** — made of one or more **passes** (two for cutover):

```
client → AUTH <token>
server → OK                                 (or "ERR auth" and disconnect)
client → SYNC
        ┌─ one pass, server-driven lazy walk of every shared folder: ───────────────┐
server →   T <count>                         file count, or 0 = unknown (no pre-count; see below)
server →   D <rel>                           create an EMPTY dir (ignored folders)
server →   B <n> + n manifest lines          a BUNDLE of small files (<=64 KB):
              <size> <mtime> <rel>             ... the manifest
client →   <want-mask>                          one char per file: '1'=send, '0'=have it
server →   per wanted file: <len> + bytes       (or "-1" if the file is locked)
server →   F <size> <mtime> <rel>            a LARGE file (>64 KB), offered on its own
client →   0 (fetch) | -1 (skip)
server →   R <bytes> + raw                      ... raw, or
         | Z + (<clen> <rlen> + bytes)…+ "0 0"  ... deflate, per-1 MB-block chunks, or
         | -1                                    ... locked: keep your copy
server → PASS-END                            └──────────────────────────────────────┘
        ── if -Cutover: server sends PING keepalives while it waits ──
server → GO                                  (operator stopped the DB and signalled)
server →   …second pass… PASS-END
server → DONE
client → BYE
```

Key points:

- **Lazy, server‑driven walk, constant memory, immediate start.** The server walks each folder
  with an explicit stack and `[IO.Directory]::EnumerateFiles/EnumerateDirectories` (lazy),
  emitting work as it goes. Nothing is enumerated up front (no whole‑tree pre‑count — that would
  add a silent delay and a second full walk), so the transfer starts at once and memory stays flat
  even for millions of files. `T` therefore carries `0` (total unknown), and progress shows
  running counts + speed without an "x of N" or ETA.
- **Small files are bundled.** Files ≤ 64 KB are batched (up to 1024 per bundle) and exchanged in
  **one round‑trip per bundle** instead of one per file: manifest → want‑mask → only the wanted
  files stream back. Over a high‑latency link this is the difference between minutes and hours.
- **Large files are streamed individually**, optionally **compressed**: each is Deflate‑compressed
  on the fly in constant‑memory 1 MB blocks (`Z`), unless compression is off or the extension is
  already‑compressed (`.zip/.jpg/.mp4/.pdf/…`) in which case it goes raw (`R`). Per‑block framing
  means the client knows each chunk's exact size and never over‑reads the shared stream.
- **Change detection** is by **size + last‑write‑time (UTC ticks)**, no content hashing. Missing
  / different size / different mtime → fetch; identical → skip. After writing, the client sets the
  file's mtime to the source's so the next sync compares correctly. A changed file is fetched
  whole (no byte‑level resume within one huge file — block‑level delta is future work).
- **Multiple folders.** Each shared folder's paths are relative to its **parent**, so the folder's
  own name becomes the top‑level directory on the receiver (`<dest>\Bars\…`, `<dest>\Ticks\…`).
- **Ignore patterns** (`.gitignore`‑style, see README). A matched file is never sent; a matched
  **directory** is not descended for files but is still **recreated empty** on the receiver (via
  `D <rel>`, recursively) — some software won't start without the folders existing.
- **Mirror is intrinsic.** The client records every path offered in the **latest pass** (the set
  resets each pass). After a clean `DONE` it deletes any local **file** under a mirrored top‑level
  folder that wasn't offered and isn't ignored — so a file removed on the source (incl. between
  cutover passes) is removed on the client. It is scoped to the copied subtrees, never deletes
  ignored content, never deletes directories, and is **skipped entirely if the sync didn't finish
  cleanly**, so a dropped connection never deletes wrongly.
- **Two‑phase cutover (`-Cutover`)** — two passes in **one session** (implies `-Once`). Pass 1
  runs while the database is live; the server then waits for the operator (keypress, or an
  `ft-cutover.go` flag file — works headless), sending `PING` keepalives so an idle NAT/firewall
  doesn't drop the connection (the idle timeout doesn't apply during the pause); pass 2 transfers
  only the delta against the now‑consistent files. If the held connection drops, just re‑run the
  client after the DB is stopped — a single pass *is* the final delta.
- **`-Once` exits only on a *clean* completion** (the client's `BYE`). A drop isn't a completion,
  so the server keeps listening and the client can reconnect and finish. A connected client that
  sends nothing for `-StallTimeout` seconds is aborted (the server keeps listening); the idle
  timeout governs only the wait **between** connections.

---

## 3. Security model

Defense in depth, from the outside in. A request must pass every layer.

### 3.1 Network reachability — Windows Firewall (OS gate)
On a hardened server inbound is denied by default, so nobody — not even the legitimate receiver —
can reach the listener until the port is opened. folder-transfer opens its port on start (needs
Administrator) and **removes the rule on exit**; with `-AllowIp` the rule is scoped to that one
source IP. `-NoFirewall` opts out.

### 3.2 Source IP — `-AllowIp` (application gate)
After a connection arrives the server checks the remote IP and drops anything else. Independent
of the firewall, so it still applies when the firewall is managed externally.

> Firewall and `-AllowIp` are **not** redundant: the firewall decides *whether packets reach the
> program*; `-AllowIp` decides *whom it serves once they do*.

### 3.3 Channel encryption — TLS 1.2
Every byte after the TCP handshake is inside a TLS session via .NET's `SslStream` (Windows
SChannel) — **vetted, not hand‑rolled**. folder-transfer never implements ciphers itself.

### 3.4 Server authenticity — certificate pinning (anti‑MITM)
The server mints a short‑lived self‑signed cert at startup (RSA‑2048, CurrentUser store, removed
on exit, subject `CN=ft-onetime`) and prints its **SHA‑256 fingerprint**, which is baked into the
generated client. The client accepts the connection **only if the presented cert's SHA‑256 matches
the pinned value** — a MITM or spoofed server (lacking the private key) is refused.

### 3.5 Client authentication — token
A random **token** (per server run) is the client's first message *inside TLS*; a mismatch rejects
the session. It is auto‑generated and baked into the generated client — client auth is always on.
The token is the only secret in the generated bat — treat that file accordingly.

### 3.6 Scope & path safety — read‑only, by construction
The folders are served **read‑only**, and **the client never sends a file path** — it only sends
fetch/skip decisions (`0`/`-1`), a bundle want‑mask, and `AUTH`/`SYNC`/`BYE`. The server opens only
the files it discovered itself. So an entire bug class is impossible *by construction*, not by
sanitising input:

- **Directory traversal** (`..\..\windows\…`) — no client‑supplied path to traverse.
- **Reserved device names** (`CON`, `PRN`, `NUL`, `COM1…9`, `LPT1…9`, `CON.txt`, …) — they aren't
  real directory entries, so they never appear in enumeration and the client can't name them.
- **ADS streams, UNC paths, trailing dots/spaces, 8.3 short names** — same reasoning.

The client additionally verifies each save path stays within the destination, so a hostile server
can't make it write outside `<destination>`.

### 3.7 Footprint & cleanup
On any normal exit (`-Once`, idle timeout, error) the server, in a `finally` block, removes its
certificate and its firewall rule. No service, user account, or global config is ever created.
After exit the machine is back to its original state.

---

## 4. Threat model & limitations

**Defended:** passive eavesdropping (TLS); server spoofing / MITM (cert pinning); unauthorised
clients on an open port (token + `-AllowIp`); path‑based attacks (the client never sends a path);
lingering exposure (one‑shot / idle auto‑shutdown + cleanup).

**Not defended / known gaps:**
- The generated bat holds the token in clear text — protect and delete it.
- No rate limiting on token guesses (mitigated by short lifetime, non‑standard port, `-AllowIp`,
  pinning).
- Integrity is size + mtime, not a content hash; equal‑size corruption isn't detected.
- A changed file is re‑fetched whole — no byte‑level resume within one huge file. Over a flaky
  WAN a very large file that drops near the end restarts from zero.
- Symlinks/junctions inside a shared folder are followed and could escape it.
- Token is compared non‑constant‑time (negligible over TLS on a short‑lived server).
- Verified end‑to‑end on Windows 11 (loopback) and over a real two‑machine WAN link; still a young
  tool — review and test in your environment before production use.

---

## 5. Design decisions

- **Why a custom PowerShell program instead of SFTP/HTTPS?** To install/alter nothing on a
  sensitive box. A real server means a new service, account, and config to manage and remove.
  folder-transfer is a plain short‑lived process that removes its own traces; .NET supplies TLS,
  so there are no third‑party dependencies. The custom layer is just framing — all crypto and file
  I/O are delegated to vetted libraries.
- **Why a streaming walk instead of a manifest?** A folder can hold millions of files; building a
  full manifest first costs memory and a slow start. Lazy enumeration starts immediately at
  constant memory. (The only up‑front pass is a cheap count for the progress bar.)
- **Why bundle small files?** The protocol is one round‑trip per item; over a high‑latency link
  thousands of tiny files are latency‑bound, not bandwidth‑bound. Bundling amortises the round‑trip
  over up to 1024 files while still preserving the per‑file delta via the want‑mask.
- **Why compress per‑block, and skip some files?** Per‑block deflate keeps memory constant and lets
  the receiver frame each chunk exactly; already‑compressed types (and tiny files) are sent raw so
  no CPU is wasted where it can't help.
- **Why does the client never send a path?** Security (3.6) and simplicity — there is nothing to
  sanitise.
- **Why recreate ignored directories empty?** Ignoring a big `log`/`cache` folder shouldn't break
  software that expects the folder to exist; the structure is cheap to recreate, the (large) file
  contents are what we skip.
- **Why is sync always a mirror?** "Sync" means the destination should match the source; a toggle
  to *not* delete just adds confusion. Deletion is scoped, skips ignored content, and only runs on
  a clean finish, so it is safe.
- **Why strict JSON config (with comments) and no auto‑repair?** Comments (`//`, `/* */`) make the
  config self‑documenting, but silently "fixing" a malformed file (a lone backslash, a trailing
  comma) would mask real mistakes — so those are reported, not repaired.

---

## 6. File map

| File | Role |
|------|------|
| `folder-transfer.bat` | Thin launcher for `ft-server.ps1` (+ `--help`, pause, no‑args prompts). |
| `ft-server.ps1` | Server engine: config, TLS, firewall, client generation, the walk (bundles / large files / empty‑dir recreation / compression), mirror logic, logging. |
| `ft-client.ps1` | Client engine: TLS + pinning, the sync client (bundles, R/Z, change detection, mirror‑delete, save‑path guard). |
| `ft-download-<name>.bat` | Generated **single self‑contained** downloader — the only file that travels to the receiver. |
| `sync.example.json` | Commented sample `-Config` file (every key). |
| `release.ps1` | Maintainer tool: build the release ZIP and publish the GitHub release. |
