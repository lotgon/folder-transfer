#requires -Version 5.1
<#
.SYNOPSIS
  Build the release ZIP and publish a GitHub release for folder-transfer.

.DESCRIPTION
  One command to cut a release: validates the version, checks the scripts are
  ASCII-only and the working tree is clean, pulls the release notes for that
  version out of CHANGELOG.md, builds dist\folder-transfer-<ver>.zip with only
  the files a user needs (the sender side), then tags, pushes, and creates the
  GitHub release with the ZIP attached.

  Assumes your changes are already committed (the version's CHANGELOG section too).

.EXAMPLE
  .\release.ps1 -Version 0.3.0
      Full release: build ZIP, tag v0.3.0, push, create the GitHub release.

.EXAMPLE
  .\release.ps1 -Version 0.3.0 -DryRun
      Build the ZIP only - no tag, no push, no publish. Good for a rehearsal.

.EXAMPLE
  .\release.ps1 -Version 0.3.1 -Force
      Proceed even if the tag exists or the tree is dirty (re-uploads the asset).
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)] [string]$Version,        # e.g. 0.3.0  (a leading 'v' is fine)
  [string]$Repo   = 'lotgon/folder-transfer',
  [string]$Branch = 'main',
  [switch]$DryRun,                                          # build the ZIP, skip tag/push/publish
  [switch]$Force                                            # tolerate a dirty tree / existing tag/release
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
Set-Location -LiteralPath $root

function Info($m) { Write-Host "[release] $m"        -ForegroundColor Cyan }
function Warn($m) { Write-Host "[release] WARN: $m"  -ForegroundColor Yellow }
function Die ($m) { Write-Host "[release] ERROR: $m" -ForegroundColor Red; exit 1 }
function Check($desc) { if ($LASTEXITCODE -ne 0) { Die "$desc failed (exit code $LASTEXITCODE)" } }

# ---- version / tag -------------------------------------------------------
$ver = $Version.TrimStart('v', 'V')
if ($ver -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { Die "version '$Version' is not SemVer (e.g. 0.3.0)" }
$tag   = "v$ver"
$title = "folder-transfer $tag"
Info "version $ver  ->  tag $tag  ->  repo $Repo"

# ---- payload: the sender-side files a user actually needs ----------------
$payload = @('folder-transfer.bat', 'ft-server.ps1', 'ft-client.ps1', 'README.md', 'LICENSE')
foreach ($f in $payload) {
  if (-not (Test-Path -LiteralPath (Join-Path $root $f))) { Die "missing release file: $f" }
}

# ---- guard: scripts must be ASCII (PS 5.1 parses in the system code page) -
foreach ($f in @('folder-transfer.bat', 'ft-server.ps1', 'ft-client.ps1')) {
  $bytes = [IO.File]::ReadAllBytes((Join-Path $root $f))
  if ($bytes | Where-Object { $_ -gt 127 } | Select-Object -First 1) {
    Die "$f contains non-ASCII bytes - scripts must be ASCII-only"
  }
}

# ---- guard: clean working tree ------------------------------------------
$dirty = @(git status --porcelain)
if ($dirty.Count -and -not $Force) {
  Die ("working tree has uncommitted changes (commit first, or pass -Force):`n" + ($dirty -join "`n"))
}

# ---- release notes from CHANGELOG.md ------------------------------------
$notesPath = Join-Path $env:TEMP "ft-relnotes-$ver.md"
$notes = New-Object System.Collections.Generic.List[string]
$changelog = Join-Path $root 'CHANGELOG.md'
if (Test-Path -LiteralPath $changelog) {
  $inSection = $false
  foreach ($ln in (Get-Content -LiteralPath $changelog -Encoding UTF8)) {
    if ($ln -match ('^##\s+\[' + [regex]::Escape($ver) + '\]')) { $inSection = $true; continue }
    if ($inSection -and $ln -match '^##\s+\[') { break }
    if ($inSection) { $notes.Add($ln) }
  }
}
$notesText = ($notes -join "`n").Trim()
if (-not $notesText) { Warn "no CHANGELOG section for [$ver] - using a generic note"; $notesText = $title }
[IO.File]::WriteAllText($notesPath, $notesText, (New-Object Text.UTF8Encoding $false))  # UTF-8, no BOM
Info "release notes -> $notesPath"

# ---- build the ZIP -------------------------------------------------------
$dist  = Join-Path $root 'dist'
$stage = Join-Path $dist "folder-transfer-$ver"
$zip   = Join-Path $dist "folder-transfer-$ver.zip"
if (Test-Path -LiteralPath $stage) { Remove-Item -Recurse -Force -LiteralPath $stage }
if (Test-Path -LiteralPath $zip)   { Remove-Item -Force -LiteralPath $zip }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
foreach ($f in $payload) { Copy-Item -LiteralPath (Join-Path $root $f) -Destination $stage }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
Info ("built {0} ({1:N1} KB)" -f $zip, ((Get-Item -LiteralPath $zip).Length / 1KB))

if ($DryRun) { Info "dry run - no tag/push/publish. ZIP ready at $zip"; return }

# ---- tooling check -------------------------------------------------------
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { Die "gh CLI not found - install GitHub CLI to publish" }

# ---- tag -----------------------------------------------------------------
if (git tag --list $tag) {
  if (-not $Force) { Die "tag $tag already exists (use -Force to reuse it)" }
  Warn "tag $tag already exists - reusing"
}
else {
  git tag -a $tag -m $title; Check "git tag $tag"
  Info "created tag $tag"
}

# ---- push branch + tag ---------------------------------------------------
Info "pushing $Branch and $tag to origin"
git push origin $Branch; Check "git push origin $Branch"
git push origin $tag;    Check "git push origin $tag"

# ---- create or update the release ---------------------------------------
# gh prints "release not found" to stderr + returns non-zero when absent; with
# ErrorActionPreference=Stop that stderr would become a terminating error, so
# silence it locally and decide purely on the exit code.
$prevEap = $ErrorActionPreference
$ErrorActionPreference = 'SilentlyContinue'
gh release view $tag --repo $Repo *> $null
$relExists = ($LASTEXITCODE -eq 0)
$ErrorActionPreference = $prevEap
if ($relExists) {
  Warn "release $tag already exists - uploading the ZIP (clobber)"
  gh release upload $tag $zip --repo $Repo --clobber; Check "gh release upload"
}
else {
  gh release create $tag $zip --repo $Repo --title $title --notes-file $notesPath; Check "gh release create"
}
Info "DONE -> https://github.com/$Repo/releases/tag/$tag"
