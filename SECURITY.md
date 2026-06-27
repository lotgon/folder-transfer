# Security Policy

folder-transfer moves data over the network and (by default) opens a firewall port, so please review
it before using it on production systems. See the **Security** section in the
[README](README.md) and the **Security model** and **Threat model & limitations** sections
in [ARCHITECTURE.md](ARCHITECTURE.md).

## Reporting a vulnerability

Please report security issues **privately**, not in public issues:

- Use GitHub's **"Report a vulnerability"** (Security → Advisories) on this repository, or
- email the maintainer (see the repository owner's profile / `LICENSE`).

Please include a description, affected version/commit, and steps to reproduce. You'll get
an acknowledgement as soon as possible.

## Scope / known limitations (already documented)

These are by‑design trade‑offs, not undisclosed bugs — see the README/ARCHITECTURE:

- The generated client `.bat` contains the auth token in clear text — treat it as a secret
  and delete it after use.
- Token attempts are not rate‑limited (mitigated by the short‑lived server, non‑standard
  port, optional `-AllowIp`, and TLS certificate pinning).
- Change detection is by size + modification time, not a content hash.
- A file copy of a *running* database is crash‑consistent at best.
- The tool runs PowerShell; on hardened hosts WDAC/AppLocker (Constrained Language Mode),
  GPO execution policy, and EDR may block or flag it.

## Supported versions

This is an early project; only the latest commit on the default branch is supported.
