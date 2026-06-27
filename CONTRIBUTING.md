# Contributing

Thanks for your interest! folder-transfer is intentionally small and dependency‑free.

## Project layout

| File | What it is |
|------|-----------|
| `folder-transfer.bat` | Thin launcher (`powershell -File ft-server.ps1 %*`). Edit rarely. |
| `ft-server.ps1` | Server engine. The bulk of the logic. |
| `ft-client.ps1` | Client engine (embedded, plain text, into each generated client `.bat`). |
| `sync.example.json` | Commented sample `-Config` file. |
| `release.ps1` | Maintainer tool — build the release ZIP and publish the GitHub release. |
| `README.md` / `ARCHITECTURE.md` / `CHANGELOG.md` | Docs. |

There is **no build step** to run the tool — the `.bat` launcher just runs the adjacent `.ps1`.
`release.ps1` only packages releases.

## Hard rules

- **ASCII only in `.ps1` / `.bat`.** Windows PowerShell 5.1 reads scripts in the system
  code page; non‑ASCII characters (e.g. Cyrillic) in a script break parsing. Non‑English text
  belongs only in Markdown docs.
- **Comments in English only** — in scripts, the JSON config/example, anywhere in the code.
- **Keep the `.bat` files thin** (no temp extraction, no base64) so the scripts stay
  readable and AppLocker/WDAC/EDR‑friendly.
- **No third‑party dependencies** — only built‑in Windows PowerShell + .NET.

## Testing

Run a real transfer locally (one PowerShell window as the server, another runs the
generated client):

```bat
folder-transfer.bat C:\some\folder -ServerHost 127.0.0.1
:: then, from download-scripts\:
ft-download-folder.bat C:\some\dest
```

Verify with hashes that source and destination match. For the two‑phase path, test
`-Cutover` and signal the cutover by creating the printed `ft-cutover.go` file.

Please run [PSScriptAnalyzer](https://github.com/PowerShell/PSScriptAnalyzer) before
opening a PR:

```powershell
Invoke-ScriptAnalyzer -Path . -Recurse
```

## Pull requests

- Keep changes focused and the tool simple.
- Update `README.md` / `ARCHITECTURE.md` / `CHANGELOG.md` when behaviour changes.
