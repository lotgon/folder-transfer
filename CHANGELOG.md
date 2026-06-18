# Changelog

All notable changes to folder-transfer are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/), and the project
aims to follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Planned
- Block‑level delta (rsync‑style) for large, slowly‑changing files.
- VSS snapshot serving for zero‑downtime consistent database copies.
- Optional hash‑based integrity verification (`-Verify`).
- Progress output with speed / ETA and a final transfer summary.
- Optional server‑side transfer log for auditing.

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

[Unreleased]: https://github.com/lotgon/folder-transfer/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/lotgon/folder-transfer/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/lotgon/folder-transfer/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/lotgon/folder-transfer/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lotgon/folder-transfer/releases/tag/v0.2.0
[0.1.0]: https://github.com/lotgon/folder-transfer/releases/tag/v0.1.0
