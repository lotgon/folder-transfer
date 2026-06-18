# ====================================================================
#  folder-transfer - CLIENT (pure PowerShell / .NET, no install)
#  Syncs the shared folder into <ToFolder>\<FolderName>: fetches changed/new
#  files (by size + last-write-time) and deletes local files that no longer
#  exist on the source (an exact mirror). Handles the server-driven two-phase
#  (cutover) sync transparently. The server's TLS fingerprint is pinned.
#  You normally do NOT run this directly - the generated ft-download-*.bat
#  passes everything in. Run that .bat:  ft-download-<name>.bat <DEST>
# ====================================================================
param(
  [string]$Server = '',
  [int]$Port = 8722,
  [string]$Token = '',
  [string]$ToFolder = '',
  [string]$Fingerprint = '',
  [switch]$Help
)
$ErrorActionPreference = 'Stop'

function Show-FetchHelp {
  Write-Host ''
  Write-Host 'folder-transfer client - syncs the shared folder into <destination>\<FolderName>.'
  Write-Host 'Normally run the generated ft-download-<name>.bat instead, e.g.:'
  Write-Host '  ft-download-ProjectX.bat D:\incoming'
  Write-Host ''
  Write-Host 'REQUIRED:'
  Write-Host '  -Server <addr>      source server address'
  Write-Host '  -ToFolder <path>    local folder to download into'
  Write-Host '  -Fingerprint <hex>  server TLS fingerprint to pin'
  Write-Host 'OPTIONAL:'
  Write-Host '  -Port <n>           TCP port (default 8722)   -Token <string>   client secret'
  Write-Host ''
}

if ($Help) { Show-FetchHelp; return }
if (-not $Server) { Write-Host 'ERROR: -Server is required.'; Show-FetchHelp; return }
if (-not $ToFolder) { Write-Host 'ERROR: -ToFolder is required.'; Show-FetchHelp; return }
if (-not $Fingerprint) { throw '-Fingerprint is required (the generated bat passes it automatically).' }
if (-not (Test-Path -LiteralPath $ToFolder)) { New-Item -ItemType Directory -Path $ToFolder | Out-Null }
$ToFolder = (Resolve-Path -LiteralPath $ToFolder).Path
$rootPrefix = $ToFolder.TrimEnd('\') + '\'

function Write-Line($s, [string]$t) { $b = [Text.Encoding]::UTF8.GetBytes($t + "`n"); $s.Write($b, 0, $b.Length); $s.Flush() }
function Read-Line($s) {
  $ms = New-Object IO.MemoryStream
  while ($true) {
    $ch = $s.ReadByte()
    if ($ch -lt 0) { if ($ms.Length -eq 0) { return $null } else { break } }
    if ($ch -eq 10) { break }
    if ($ch -ne 13) { $ms.WriteByte([byte]$ch) }
  }
  return [Text.Encoding]::UTF8.GetString($ms.ToArray())
}

$script:ExpectedFp = $Fingerprint.Replace(':', '').Replace('-', '').ToLower()

$tcp = [System.Net.Sockets.TcpClient]::new()
$tcp.Connect($Server, $Port)
$raw = $tcp.GetStream()
$stream = $raw
try {
  $cb = [System.Net.Security.RemoteCertificateValidationCallback] {
    param($sndr, $crt, $chain, $err)
    $h = [Security.Cryptography.SHA256]::Create().ComputeHash($crt.GetRawCertData())
    return ((([BitConverter]::ToString($h)).Replace('-', '').ToLower()) -eq $script:ExpectedFp)
  }
  $stream = New-Object System.Net.Security.SslStream($raw, $false, $cb)
  $stream.AuthenticateAsClient('ft-onetime')

  Write-Line $stream ("AUTH " + $Token)
  if ((Read-Line $stream) -ne 'OK') { throw 'auth failed / rejected by server' }

  Write-Host "[fetch] sync -> $ToFolder"
  $got = 0; $skipped = 0; $bytes = 0; $pass = 0; $deleted = 0
  $seen = New-Object 'System.Collections.Generic.HashSet[string]'
  $mirrorRoot = $null      # <ToFolder>\<FolderName>, derived from the offered paths
  $more = $true; $syncOk = $false

  Write-Line $stream 'SYNC'
  while ($more) {
    $pass++
    $total = [int64]0      # file count for this pass (sent by the server as 'T <n>')
    $lastProg = Get-Date
    $seen.Clear()   # mirror must reflect ONLY the latest pass, so files deleted
                    # between cutover pass 1 and pass 2 get removed on the client
    while ($true) {
      $h = Read-Line $stream
      if ($null -eq $h) { $more = $false; break }
      if ($h -eq 'PASS-END') { break }
      if ($h -match '^T (\d+)$') { $total = [int64]$matches[1]; continue }   # server's file count
      if ($h -notmatch '^F (\d+) (\d+) (.+)$') { continue }
      $size = [int64]$matches[1]; $mt = [int64]$matches[2]; $rel = $matches[3]
      $target = [IO.Path]::GetFullPath((Join-Path $ToFolder $rel))
      if (-not ($target.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase))) {
        Write-Host ("  skip unsafe path from server: {0}" -f $rel); Write-Line $stream '-1'; continue
      }
      if (-not $mirrorRoot) {
        $top = ($rel -split '[\\/]')[0]
        if ($top) { $mirrorRoot = [IO.Path]::GetFullPath((Join-Path $ToFolder $top)) }
      }
      [void]$seen.Add($target.ToLowerInvariant())
      if (((Get-Date) - $lastProg).TotalSeconds -ge 2) {
        $lastProg = Get-Date
        $done = $got + $skipped; $left = $total - $done; if ($left -lt 0) { $left = 0 }
        Write-Host ("[fetch] progress: {0}/{1} ({2} left) - fetched {3}, unchanged {4}, {5:N1} MB" -f $done, $total, $left, $got, $skipped, ($bytes / 1MB))
      }
      $need = $true
      if (Test-Path -LiteralPath $target) {
        $li = Get-Item -LiteralPath $target
        if ($li.Length -eq $size -and $li.LastWriteTimeUtc.Ticks -eq $mt) { $need = $false }
      }
      if (-not $need) { Write-Line $stream '-1'; $skipped++; continue }
      Write-Line $stream '0'                       # changed/new -> full fetch (overwrite)
      $remain = [int64](Read-Line $stream)
      if ($remain -lt 0) { continue }
      $dir = Split-Path $target -Parent
      if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
      $fs = [IO.File]::Open($target, [IO.FileMode]::Create, [IO.FileAccess]::Write)
      try {
        $buf = New-Object byte[] 1048576; $left = $remain
        while ($left -gt 0) {
          $n = $stream.Read($buf, 0, [Math]::Min($buf.Length, $left))
          if ($n -le 0) { throw "connection closed early on $rel" }
          $fs.Write($buf, 0, $n); $left -= $n
        }
      } finally { $fs.Close() }
      try { (Get-Item -LiteralPath $target).LastWriteTimeUtc = [DateTime]::new($mt, [DateTimeKind]::Utc) } catch {}
      $bytes += $remain; $got++
    }
    if (-not $more) { break }
    $dir2 = $null
    while ($true) { $dir2 = Read-Line $stream; if ($null -eq $dir2) { $more = $false; break }; if ($dir2 -ne 'PING') { break } }
    if (-not $more) { break }
    if ($dir2 -eq 'GO') { Write-Host '[fetch] server signalled phase 2 (final sync)'; continue }
    if ($dir2 -eq 'DONE') { $syncOk = $true }
    break
  }

  # Mirror: delete local files no longer on the source. Only after a CLEAN finish
  # and only against the LAST pass's file set, so a drop never deletes wrongly.
  if ($syncOk -and $mirrorRoot -and (Test-Path -LiteralPath $mirrorRoot)) {
    Get-ChildItem -LiteralPath $mirrorRoot -Recurse -File | ForEach-Object {
      if (-not $seen.Contains($_.FullName.ToLowerInvariant())) {
        Remove-Item -LiteralPath $_.FullName -Force -EA SilentlyContinue; $deleted++
      }
    }
  }
  elseif (-not $syncOk) { Write-Host '[fetch] sync did not finish cleanly - nothing deleted' }

  Write-Line $stream 'BYE'
  Write-Host ("[fetch] sync done. passes={0} fetched={1} unchanged={2} deleted={3} bytes={4:N0}" -f $pass, $got, $skipped, $deleted, $bytes)
}
finally { $tcp.Close() }
