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
  [string]$Ignore = '',          # same ignore patterns as the server (baked in) - so mirror won't delete them
  [switch]$Help
)
$ErrorActionPreference = 'Stop'
$ignorePatterns = @($Ignore -split '[;,]' | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne '' })

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
function Format-Span($sec) {
  if ($sec -lt 0 -or $sec -gt 359999) { return '?' }
  return ('{0:hh\:mm\:ss}' -f [TimeSpan]::FromSeconds([int]$sec))
}
function Show-FetchProgress($done, $total, $got, $skipped, $bytes) {
  # throttled (~2s) progress; called between files AND mid-file so big files don't look frozen
  $nowt = Get-Date
  if (($nowt - $script:lastProg).TotalSeconds -lt 2) { return }
  $dt = ($nowt - $script:lastProg).TotalSeconds
  $spd = if ($dt -gt 0) { (($bytes - $script:lastBytes) / 1MB) / $dt } else { 0 }
  $el = ($nowt - $script:passStart).TotalSeconds
  $rate = if ($el -gt 0) { $done / $el } else { 0 }
  $left = $total - $done; if ($left -lt 0) { $left = 0 }
  $eta = if ($rate -gt 0) { Format-Span ($left / $rate) } else { '?' }
  Write-Host ("[fetch] progress: {0}/{1} ({2} left) - fetched {3}, unchanged {4}, {5:N1} MB @ {6:N1} MB/s, ETA {7}" -f $done, $total, $left, $got, $skipped, ($bytes / 1MB), $spd, $eta)
  $script:lastProg = $nowt; $script:lastBytes = $bytes
}
$script:RxCache = @{}
function Convert-GlobToRegex([string]$glob) {
  # glob with '/' separators -> anchored regex. * = within a segment, ** = any depth, ? = one char.
  if ($script:RxCache.ContainsKey($glob)) { return $script:RxCache[$glob] }
  $sb = New-Object Text.StringBuilder; [void]$sb.Append('^'); $i = 0
  while ($i -lt $glob.Length) {
    $c = $glob[$i]
    if ($c -eq '*') {
      if (($i + 1) -lt $glob.Length -and $glob[$i + 1] -eq '*') { [void]$sb.Append('.*'); $i += 2 }
      else { [void]$sb.Append('[^/]*'); $i++ }
    }
    elseif ($c -eq '?') { [void]$sb.Append('[^/]'); $i++ }
    else { [void]$sb.Append([Regex]::Escape([string]$c)); $i++ }
  }
  [void]$sb.Append('$')
  $rx = New-Object Text.RegularExpressions.Regex($sb.ToString(), [Text.RegularExpressions.RegexOptions]::IgnoreCase)
  $script:RxCache[$glob] = $rx
  return $rx
}
function Test-IgnoredRel([string]$rel, [bool]$isDir, $patterns) {
  # Is this item (or any ancestor dir) ignored? Mirrors the server so the client's mirror
  # step never deletes ignored content. Same rules: trailing '/' = dirs only; a body without
  # '/' is a NAME pattern (any segment); a body with '/' is a PATH pattern anchored at the root
  # (* within a segment, ** any depth).
  if (-not $patterns -or @($patterns).Count -eq 0) { return $false }
  $rel = ($rel -replace '\\', '/').Trim('/')
  if (-not $rel) { return $false }
  $segs = $rel -split '/'
  foreach ($p in $patterns) {
    $body = ($p -replace '\\', '/'); $dirOnly = $body.EndsWith('/'); $body = $body.Trim('/')
    if (-not $body) { continue }
    if ($body.Contains('/')) {
      $rx = Convert-GlobToRegex $body
      for ($i = 1; $i -le $segs.Count; $i++) {
        $isSegDir = ($i -lt $segs.Count) -or $isDir
        if ($dirOnly -and -not $isSegDir) { continue }
        if ($rx.IsMatch(($segs[0..($i - 1)] -join '/'))) { return $true }
      }
    }
    else {
      for ($i = 0; $i -lt $segs.Count; $i++) {
        $isSegDir = ($i -lt ($segs.Count - 1)) -or $isDir
        if ($dirOnly -and -not $isSegDir) { continue }
        if ($segs[$i] -like $body) { return $true }
      }
    }
  }
  return $false
}

$script:ExpectedFp = $Fingerprint.Replace(':', '').Replace('-', '').ToLower()

$tcp = [System.Net.Sockets.TcpClient]::new()
try { $tcp.NoDelay = $true } catch {}   # disable Nagle - huge win for many small files over WAN
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
  $mirrorRoots = New-Object 'System.Collections.Generic.HashSet[string]'   # <ToFolder>\<top> per shared folder
  $more = $true; $syncOk = $false
  $runStart = Get-Date

  Write-Line $stream 'SYNC'
  while ($more) {
    $pass++
    $total = [int64]0      # file count for this pass (sent by the server as 'T <n>')
    $script:passStart = Get-Date; $script:lastProg = $script:passStart; $script:lastBytes = 0
    $seen.Clear()   # mirror must reflect ONLY the latest pass, so files deleted
                    # between cutover pass 1 and pass 2 get removed on the client
    while ($true) {
      $h = Read-Line $stream
      if ($null -eq $h) { $more = $false; break }
      if ($h -eq 'PASS-END') { break }
      if ($h -match '^T (\d+)$') { $total = [int64]$matches[1]; continue }   # server's file count
      if ($h -match '^D (.+)$') {
        # create an (empty) directory - used for ignored folders so the structure still exists
        $drel = $matches[1]
        $dt = [IO.Path]::GetFullPath((Join-Path $ToFolder $drel))
        if ($dt.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
          if (-not (Test-Path -LiteralPath $dt)) { New-Item -ItemType Directory -Path $dt -Force | Out-Null }
          $top = ($drel -split '[\\/]')[0]
          if ($top) { [void]$mirrorRoots.Add([IO.Path]::GetFullPath((Join-Path $ToFolder $top))) }
        }
        continue
      }
      if ($h -match '^B (\d+)$') {
        # bundle of small files: read the manifest, reply with a want-mask, receive the wanted ones
        $bcount = [int]$matches[1]
        $bitems = New-Object System.Collections.Generic.List[object]
        for ($k = 0; $k -lt $bcount; $k++) {
          $ml = Read-Line $stream
          if ($ml -match '^(\d+) (\d+) (.+)$') { $bitems.Add([pscustomobject]@{ Size = [int64]$matches[1]; Mt = [int64]$matches[2]; Rel = $matches[3] }) }
          else { $bitems.Add($null) }
        }
        $sbm = New-Object Text.StringBuilder
        $btargets = New-Object System.Collections.Generic.List[object]
        foreach ($it in $bitems) {
          if ($null -eq $it) { [void]$sbm.Append('0'); $btargets.Add($null); continue }
          $bt = [IO.Path]::GetFullPath((Join-Path $ToFolder $it.Rel))
          if (-not ($bt.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase))) { [void]$sbm.Append('0'); $btargets.Add($null); continue }
          $top = ($it.Rel -split '[\\/]')[0]
          if ($top) { [void]$mirrorRoots.Add([IO.Path]::GetFullPath((Join-Path $ToFolder $top))) }
          [void]$seen.Add($bt.ToLowerInvariant())
          $need = $true
          if (Test-Path -LiteralPath $bt) { $li = Get-Item -LiteralPath $bt; if ($li.Length -eq $it.Size -and $li.LastWriteTimeUtc.Ticks -eq $it.Mt) { $need = $false } }
          if ($need) { [void]$sbm.Append('1'); $btargets.Add($bt) } else { [void]$sbm.Append('0'); $skipped++; $btargets.Add($null) }
        }
        Write-Line $stream $sbm.ToString()
        for ($k = 0; $k -lt $bcount; $k++) {
          if ($null -eq $btargets[$k]) { continue }
          $len = [int64](Read-Line $stream)
          if ($len -lt 0) { continue }   # server could not provide it (locked) -> keep our copy
          $bt = $btargets[$k]
          $bdir = Split-Path $bt -Parent
          if (-not (Test-Path -LiteralPath $bdir)) { New-Item -ItemType Directory -Path $bdir -Force | Out-Null }
          $bfs = [IO.File]::Open($bt, [IO.FileMode]::Create, [IO.FileAccess]::Write)
          try {
            $buf = New-Object byte[] 65536; $left = $len
            while ($left -gt 0) {
              $n = $stream.Read($buf, 0, [Math]::Min($buf.Length, $left))
              if ($n -le 0) { throw "connection closed early (bundle) on $($bitems[$k].Rel)" }
              $bfs.Write($buf, 0, $n); $left -= $n; $bytes += $n
            }
          } finally { $bfs.Close() }
          try { (Get-Item -LiteralPath $bt).LastWriteTimeUtc = [DateTime]::new($bitems[$k].Mt, [DateTimeKind]::Utc) } catch {}
          $got++
          Show-FetchProgress ($got + $skipped) $total $got $skipped $bytes
        }
        continue
      }
      if ($h -notmatch '^F (\d+) (\d+) (.+)$') { continue }
      $size = [int64]$matches[1]; $mt = [int64]$matches[2]; $rel = $matches[3]
      $target = [IO.Path]::GetFullPath((Join-Path $ToFolder $rel))
      if (-not ($target.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase))) {
        Write-Host ("  skip unsafe path from server: {0}" -f $rel); Write-Line $stream '-1'; continue
      }
      $top = ($rel -split '[\\/]')[0]
      if ($top) { [void]$mirrorRoots.Add([IO.Path]::GetFullPath((Join-Path $ToFolder $top))) }
      [void]$seen.Add($target.ToLowerInvariant())
      Show-FetchProgress ($got + $skipped) $total $got $skipped $bytes
      $need = $true
      if (Test-Path -LiteralPath $target) {
        $li = Get-Item -LiteralPath $target
        if ($li.Length -eq $size -and $li.LastWriteTimeUtc.Ticks -eq $mt) { $need = $false }
      }
      if (-not $need) { Write-Line $stream '-1'; $skipped++; continue }
      Write-Line $stream '0'                       # changed/new -> full fetch (overwrite)
      $hdr = Read-Line $stream
      if ($hdr -eq '-1') { continue }              # server can't provide it (locked) -> keep our copy
      $dir = Split-Path $target -Parent
      if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
      $fs = [IO.File]::Open($target, [IO.FileMode]::Create, [IO.FileAccess]::Write)
      try {
        if ($hdr -eq 'Z') {
          # compressed: deflate chunks "<clen> <rlen>" + clen bytes, ended by "0 0"
          while ($true) {
            $cinfo = (Read-Line $stream) -split ' '
            $clen = [int]$cinfo[0]; $rlen = [int]$cinfo[1]
            if ($clen -le 0) { break }
            $cbuf = New-Object byte[] $clen; $cgot = 0
            while ($cgot -lt $clen) {
              $n = $stream.Read($cbuf, $cgot, $clen - $cgot)
              if ($n -le 0) { throw "connection closed early (compressed) on $rel" }
              $cgot += $n
            }
            $cms = New-Object IO.MemoryStream(, $cbuf)
            $dz = New-Object IO.Compression.DeflateStream($cms, [IO.Compression.CompressionMode]::Decompress)
            $obuf = New-Object byte[] $rlen; $off = 0
            while ($off -lt $rlen) { $n = $dz.Read($obuf, $off, $rlen - $off); if ($n -le 0) { break }; $off += $n }
            $dz.Close(); $cms.Dispose()
            $fs.Write($obuf, 0, $off); $bytes += $off
            Show-FetchProgress ($got + $skipped) $total $got $skipped $bytes
          }
        }
        else {
          # raw: header "R <bytes>"
          $remain = [int64](($hdr -split ' ')[1])
          $buf = New-Object byte[] 1048576; $left = $remain
          while ($left -gt 0) {
            $n = $stream.Read($buf, 0, [Math]::Min($buf.Length, $left))
            if ($n -le 0) { throw "connection closed early on $rel" }
            $fs.Write($buf, 0, $n); $left -= $n; $bytes += $n
            Show-FetchProgress ($got + $skipped) $total $got $skipped $bytes
          }
        }
      } finally { $fs.Close() }
      try { (Get-Item -LiteralPath $target).LastWriteTimeUtc = [DateTime]::new($mt, [DateTimeKind]::Utc) } catch {}
      $got++
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
  # Ignored content is never deleted. Runs per shared folder (its own subtree).
  if ($syncOk) {
    foreach ($mr in $mirrorRoots) {
      if (-not (Test-Path -LiteralPath $mr)) { continue }
      Get-ChildItem -LiteralPath $mr -Recurse -File | ForEach-Object {
        $rel2 = $_.FullName.Substring($rootPrefix.Length)
        if (Test-IgnoredRel $rel2 $false $ignorePatterns) { return } # never delete ignored content
        if (-not $seen.Contains($_.FullName.ToLowerInvariant())) {
          Remove-Item -LiteralPath $_.FullName -Force -EA SilentlyContinue; $deleted++
        }
      }
    }
  }
  else { Write-Host '[fetch] sync did not finish cleanly - nothing deleted' }

  Write-Line $stream 'BYE'
  $secs = ((Get-Date) - $runStart).TotalSeconds
  $avg = if ($secs -gt 0) { ($bytes / 1MB) / $secs } else { 0 }
  Write-Host ("[fetch] sync done. passes={0} fetched={1} unchanged={2} deleted={3} bytes={4:N0} in {5} @ {6:N1} MB/s avg" -f $pass, $got, $skipped, $deleted, $bytes, (Format-Span $secs), $avg)
}
finally { $tcp.Close() }
