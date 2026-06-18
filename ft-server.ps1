# ====================================================================
#  folder-transfer - SERVER  (pure PowerShell / .NET, no install)
#  Serves ONE folder read-only over TLS, then auto-exits on idle
#  timeout or after a completed session (-Once). Leaves NOTHING behind:
#  no service, no user, no system config, ephemeral in-memory cert.
#
#  Security (all optional except TLS):
#    -Token <s>    one-time shared token the client must send
#    -AllowIp <ip> restrict to a single client IP (empty = any)
#    -Plain        disable TLS (loopback testing only)
# ====================================================================
param(
  [Parameter(Position = 0)] [string]$Folder = '',   # REQUIRED, positional: folder-transfer.bat <folder> ...
  [int]$Port = 8722,
  [string]$AllowIp = '',
  [int]$IdleSeconds = 600,
  [int]$StallTimeout = 120,     # abort a connected client that sends no data for this many seconds
  [switch]$Once,
  [string]$ServerHost = '',     # address baked into the generated client (auto-detected if empty)
  [string]$ClientOut = '',      # where to write the client (default: .\download-scripts\ft-download-<Folder>.bat)
  [switch]$NoFirewall,          # do NOT touch the firewall (by default the port IS opened on start, needs admin)
  [switch]$Cutover,             # TWO-PHASE sync: pass 1, wait for the operator to stop the DB, pass 2 (implies -Once)
  [switch]$Help
)
if ($Cutover) { $Once = $true }   # a two-phase cutover is one session: exit after phase 2
$ErrorActionPreference = 'Stop'

function Log($m) { Write-Host ("[serve {0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $m) }

function Show-ServeHelp {
  Write-Host ''
  Write-Host 'folder-transfer server - serves ONE folder over TLS and syncs it to the receiver.'
  Write-Host 'Run via:  folder-transfer.bat <FOLDER> [options]'
  Write-Host ''
  Write-Host 'TWO MODES:'
  Write-Host '  (default)           single-phase sync - copy the folder; re-run to sync changes'
  Write-Host '  -Cutover            two-phase sync - pass 1 (DB live), you stop the DB, pass 2 (final). Implies -Once.'
  Write-Host ''
  Write-Host 'REQUIRED (positional - just the path, first argument):'
  Write-Host '  <FOLDER>            folder to share, read-only   e.g.  folder-transfer.bat D:\data'
  Write-Host ''
  Write-Host 'OPTIONAL:'
  Write-Host '  -ServerHost <addr>  source address baked into the client   (default: auto IPv4)'
  Write-Host '  -Port <n>           TCP port                               (default: 8722)'
  Write-Host '  -AllowIp <ip>       allow only this client IP              (default: any)'
  Write-Host '  -IdleSeconds <n>    auto-close after N seconds with NO client connected (default: 600)'
  Write-Host '  -StallTimeout <n>   abort a connected client that sends nothing for N seconds (default: 120)'
  Write-Host '  -Once               close after the first successful transfer (default: off)'
  Write-Host '  -ClientOut <path>   where to write the client bat          (default: .\download-scripts\ft-download-<Folder>.bat)'
  Write-Host '  -NoFirewall         do NOT touch Firewall (default: port IS opened on start + closed on exit; needs admin)'
  Write-Host '  -Help               this help'
  Write-Host ''
  Write-Host 'Examples:'
  Write-Host '  folder-transfer.bat                                         (no args - asks interactively)'
  Write-Host '  folder-transfer.bat D:\data                                 (simplest - just the folder)'
  Write-Host '  folder-transfer.bat D:\data -ServerHost 10.0.0.5 -Once       (single-phase sync)'
  Write-Host '  folder-transfer.bat D:\db   -ServerHost 10.0.0.5 -Cutover    (two-phase DB sync)'
  Write-Host ''
  Write-Host 'The token (client secret) is auto-generated and baked into the client - you'
  Write-Host 'never set it. Parameter names are case-insensitive; the folder is positional.'
  Write-Host ''
}

if ($Help) { Show-ServeHelp; return }
if (-not $Folder) {
  # No folder on the command line (e.g. double-clicked) - ask a couple of questions,
  # the same way the generated client asks for its destination.
  Write-Host ''
  Write-Host 'No folder given - a couple of quick questions (press Enter for the default).'
  Write-Host ''
  $Folder = (Read-Host '1) Folder to share / sync (full path)').Trim().Trim('"')
  $modeAns = (Read-Host '2) Mode: [1] single-phase sync (default) or [2] cutover (two-phase, for a live DB)').Trim()
  if ($modeAns -eq '2') { $Cutover = $true; $Once = $true; Write-Host '   -> cutover mode (two passes; implies -Once)' }
  else { Write-Host '   -> single-phase sync' }
  Write-Host ''
}
if (-not $Folder) { Write-Host 'ERROR: a folder is required (the folder to share).'; Show-ServeHelp; return }
if (-not (Test-Path -LiteralPath $Folder)) { Write-Host "ERROR: folder path not found: $Folder"; return }
$Folder = (Resolve-Path -LiteralPath $Folder).Path

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
function Write-ClientBat($OutPath, $Hostt, $Portt, $Tok, $Fpr, $FetchSrc) {
  # Generate a SINGLE self-contained client .bat (the only file you carry to the
  # receiver): a batch header with the baked connection details, then the PLAIN
  # PowerShell client body after a marker. The header writes that body to a temp
  # .ps1 and runs it. No base64 / obfuscation - it is all readable text.
  $tpl = @'
@echo off
setlocal EnableExtensions
rem ====================================================================
rem  folder-transfer downloader (generated, self-contained). Usage: thisfile.bat [DEST_FOLDER]
rem  If you omit DEST_FOLDER you will be asked for it (Enter = the current folder).
rem  ONE file - copy just this to the receiver. The shared folder is recreated
rem  by name inside DEST. Connection details are baked in; the client follows
rem  below as plain text.
rem ====================================================================
set "SERVER=__HOST__"
set "PORT=__PORT__"
set "TOKEN=__TOKEN__"
set "FP=__FP__"
set "DEST=%~1"
if not "%DEST%"=="" goto :gotdest
echo Destination folder not given. Where should the folder be synced to?
set /p "DEST=Destination [%CD%]: "
if "%DEST%"=="" set "DEST=%CD%"
:gotdest
echo Downloading from %SERVER%:%PORT% into "%DEST%" ...
set "PS1=%TEMP%\ftdl_%RANDOM%%RANDOM%.ps1"
powershell -NoProfile -Command "$c=Get-Content -Raw -LiteralPath '%~f0'; $m='#'+'FTPSBODY#'; [IO.File]::WriteAllText($env:PS1, $c.Substring($c.IndexOf($m)+$m.Length))"
powershell -NoProfile -ExecutionPolicy Bypass -File "%PS1%" -Server "%SERVER%" -Port %PORT% -Token "%TOKEN%" -ToFolder "%DEST%" -Fingerprint "%FP%"
set "RC=%errorlevel%"
del "%PS1%" >nul 2>&1
if not "%RC%"=="0" ( echo [ERROR] Download failed ^(code %RC%^). Re-run to resume. ) else ( echo [OK] Done -^> %DEST% )
echo.
echo Transfer finished with exit code %RC%. Review the messages above.
pause
endlocal & exit /b %RC%
#FTPSBODY#
'@
  $bat = $tpl.Replace('__HOST__', $Hostt).Replace('__PORT__', [string]$Portt).Replace('__TOKEN__', $Tok).Replace('__FP__', $Fpr)
  $bat = ($bat + "`r`n" + $FetchSrc) -replace "`r?`n", "`r`n"
  Set-Content -LiteralPath $OutPath -Value $bat -Encoding ASCII
}

function Send-Pass($s, $root) {
  # One lazy walk of the tree. For each file: send "F <size> <mtimeTicks> <rel>",
  # read the client's reply (byte offset to start at, or -1 to skip), send the
  # remaining bytes. Paths are relative to the PARENT of the shared folder so the
  # folder's own name is preserved on the client. Returns a stats object; .Ok is
  # $false if the client disconnected mid-pass. The client never sends a path ->
  # no traversal / reserved device names by construction.
  $st = [pscustomobject]@{ Ok = $true; Offered = 0; Sent = 0; Skipped = 0; Bytes = [int64]0 }
  $base = Split-Path $root -Parent
  if (-not $base) { $base = $root }
  $stack = New-Object System.Collections.Stack
  $stack.Push($root)
  while ($stack.Count -gt 0) {
    $dir = $stack.Pop()
    try { foreach ($sd in [IO.Directory]::EnumerateDirectories($dir)) { $stack.Push($sd) } } catch {}
    $en = $null
    try { $en = [IO.Directory]::EnumerateFiles($dir).GetEnumerator() } catch { $en = $null }
    if (-not $en) { continue }
    while ($en.MoveNext()) {
      $full = $en.Current
      $fi = $null
      try { $fi = [IO.FileInfo]::new($full) } catch { $fi = $null }
      if (-not $fi) { continue }
      $rel = $full.Substring($base.Length).TrimStart('\', '/')
      Write-Line $s ("F {0} {1} {2}" -f $fi.Length, $fi.LastWriteTimeUtc.Ticks, $rel)
      $st.Offered++
      $resp = Read-Line $s
      if ($null -eq $resp) { $st.Ok = $false; return $st }
      $offset = [int64]0
      if (-not [int64]::TryParse($resp, [ref]$offset)) { $offset = [int64](-1) }
      if ($offset -lt 0) { $st.Skipped++; continue }
      # Open with a permissive share mode so files another process holds open
      # (e.g. a live database's data/log files during cutover pass 1) can still
      # be read. If the file is locked exclusively, do NOT abort the session:
      # send -1 so the client keeps its current copy (no truncation, no delete)
      # and move on. In cutover, pass 2 (DB stopped) picks it up consistently.
      $fs = $null
      try {
        $fs = New-Object IO.FileStream($full, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]'ReadWrite, Delete'))
      }
      catch {
        Log ("session: cannot read (in use?), skipping this pass: {0} -- {1}" -f $rel, $_.Exception.Message)
        Write-Line $s '-1'
        $st.Skipped++
        continue
      }
      try {
        $remain = $fs.Length - $offset; if ($remain -lt 0) { $remain = 0 }
        Write-Line $s ([string]$remain)
        if ($offset -gt 0) { [void]$fs.Seek($offset, 'Begin') }
        $buf = New-Object byte[] 1048576; $left = $remain
        while ($left -gt 0) {
          $n = $fs.Read($buf, 0, [Math]::Min($buf.Length, $left))
          if ($n -le 0) { break }
          $s.Write($buf, 0, $n); $left -= $n
        }
        $s.Flush()
        $st.Sent++; $st.Bytes += $remain
      } finally { $fs.Close() }
    }
  }
  return $st
}

function Wait-Cutover($s) {
  # Pause between pass 1 and pass 2: the operator stops the database, then
  # signals us by pressing a key at this console OR creating the .go file.
  # PING lines keep the idle TLS connection alive across the pause.
  $goFlag = Join-Path $PSScriptRoot 'ft-cutover.go'
  Remove-Item -LiteralPath $goFlag -Force -EA SilentlyContinue
  Write-Host ''
  Write-Host '========================================================================'
  Write-Host ' PHASE 1 complete. Now STOP THE DATABASE so its files are consistent.'
  Write-Host " Then press any key here, or create the file:"
  Write-Host "   $goFlag"
  Write-Host '========================================================================'
  $ticks = 0
  while ($true) {
    if (Test-Path -LiteralPath $goFlag) { Remove-Item -LiteralPath $goFlag -Force -EA SilentlyContinue; break }
    $hasKey = $false
    try { $hasKey = [Console]::KeyAvailable } catch { $hasKey = $false }
    if ($hasKey) { try { [void][Console]::ReadKey($true) } catch {}; break }
    Start-Sleep -Milliseconds 250; $ticks++
    if (($ticks % 60) -eq 0) { Write-Line $s 'PING' }   # keepalive ~every 15s
  }
  Log 'cutover signal received - running final sync pass'
}

$CertSubject = 'CN=ft-onetime'
function Remove-FtCerts {
  Get-ChildItem Cert:\CurrentUser\My -EA SilentlyContinue |
    Where-Object { $_.Subject -eq $CertSubject } |
    ForEach-Object { Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $_.Thumbprint) -Force -EA SilentlyContinue }
}
function New-ServerCert {
  # SChannel (Windows TLS) needs the key in a real key container, so we
  # mint a short-lived self-signed cert in the user store (no admin) and
  # delete it again on exit.
  Remove-FtCerts
  return New-SelfSignedCertificate -Type Custom -Subject $CertSubject `
    -KeyAlgorithm RSA -KeyLength 2048 -KeyExportPolicy Exportable `
    -CertStoreLocation 'Cert:\CurrentUser\My' -NotAfter (Get-Date).AddDays(2) `
    -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.1')
}

# Token = the client's secret. Always auto-generated (random) and baked into the
# generated client, so you never have to think about it.
$cs = 'ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789'
$rb = New-Object byte[] 24
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($rb)
$Token = -join ($rb | ForEach-Object { $cs[$_ % $cs.Length] })
Log 'token: auto-generated (baked into the client - you do not need to know it)'

$cert = New-ServerCert
$sha = [Security.Cryptography.SHA256]::Create().ComputeHash($cert.RawData)
$fp = ([BitConverter]::ToString($sha)).Replace('-', '').ToLower()
Write-Host "FINGERPRINT=$fp"
Log "TLS on. Client must pin this fingerprint (-Fingerprint $fp)"

if (-not $ServerHost) {
  try { $ServerHost = (Get-NetIPConfiguration | Where-Object { $_.IPv4DefaultGateway -ne $null } | Select-Object -First 1 -ExpandProperty IPv4Address).IPAddress } catch {}
  if (-not $ServerHost) { $ServerHost = 'THIS-SERVER-IP' }
}
if (-not $ClientOut) {
  # default: a 'download-scripts' subfolder NEXT TO this tool (where you run
  # from). One bat per shared folder accumulates here, reusable later.
  $leaf = Split-Path $Folder -Leaf
  if (-not $leaf) { $leaf = 'Share' }
  $ClientOut = Join-Path (Join-Path $PSScriptRoot 'download-scripts') ("ft-download-{0}.bat" -f $leaf)
}
$outDir = Split-Path $ClientOut -Parent
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) {
  try { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
  catch { Log "WARN: could not create $outDir : $_" }
}
$fetchPath = Join-Path $PSScriptRoot 'ft-client.ps1'
if (Test-Path -LiteralPath $fetchPath) {
  Write-ClientBat $ClientOut $ServerHost $Port $Token $fp (Get-Content -Raw -LiteralPath $fetchPath)
  $modeLabel = if ($Cutover) { 'two-phase sync (cutover)' } else { 'single-phase sync' }
  Log "CLIENT WRITTEN ($modeLabel) -> $ClientOut"
  Log "    this is ONE self-contained file - copy just it to the receiver"
}
else { Log "WARN: ft-client.ps1 not found next to the tool - client .bat NOT generated" }

$fwRule = "ft-temp-$Port"
$fwOpened = $false
# Firewall is opened BY DEFAULT (skip with -NoFirewall).
$doFirewall = (-not $NoFirewall)
if ($doFirewall) {
  $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
  if ($isAdmin) {
    # clean any rule left over from a previous crashed run, then open fresh
    Get-NetFirewallRule -DisplayName $fwRule -EA SilentlyContinue | Remove-NetFirewallRule -EA SilentlyContinue
    $fwArgs = @{ DisplayName = $fwRule; Direction = 'Inbound'; Action = 'Allow'; Protocol = 'TCP'; LocalPort = $Port }
    if ($AllowIp) { $fwArgs['RemoteAddress'] = $AllowIp }
    try {
      New-NetFirewallRule @fwArgs | Out-Null; $fwOpened = $true
      Log ("firewall OPENED: TCP {0} {1}" -f $Port, $(if ($AllowIp) { "from $AllowIp" } else { '(any source)' }))
    } catch { Log "WARN: could not open firewall: $_" }
  }
  else {
    Log ("WARN: not elevated - firewall NOT opened. Run as Administrator to auto-open TCP {0}, open it manually, or pass -NoFirewall to silence this." -f $Port)
  }
}

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, $Port)
$listener.Start()
Log ("listening 0.0.0.0:{0}  root={1}  idle={2}s  once={3}" -f $Port, $Folder, $IdleSeconds, [bool]$Once)

$sessionNum = 0
try {
  while ($true) {
    Log ("idle: waiting for a client - will auto-close in {0}s if none connects" -f $IdleSeconds)
    $waited = 0
    while (-not $listener.Pending()) {
      Start-Sleep -Milliseconds 200; $waited += 200
      if ($waited -ge $IdleSeconds * 1000) { Log ("idle timeout reached ({0}s with no client) - shutting down" -f $IdleSeconds); return }
    }
    $client = $listener.AcceptTcpClient()
    $sessionNum++
    $remote = $client.Client.RemoteEndPoint.ToString()           # IP:port
    $ip = $client.Client.RemoteEndPoint.Address.ToString()
    $t0 = Get-Date
    $raw = $client.GetStream()
    $stream = $raw
    $completed = $false
    try {
      if ($AllowIp -and $ip -ne $AllowIp) { Log ("session #{0} REJECTED from {1} (only {2} is allowed)" -f $sessionNum, $remote, $AllowIp); continue }
      $stream = New-Object System.Net.Security.SslStream($raw, $false)
      $stream.AuthenticateAsServer($cert, $false, [System.Security.Authentication.SslProtocols]::Tls12, $false)
      try { $stream.ReadTimeout = $StallTimeout * 1000 } catch {}   # abort a stalled client
      Log ("session #{0} connected from {1} (TLS ok; stall timeout {2}s)" -f $sessionNum, $remote, $StallTimeout)
      $line = Read-Line $stream
      if ($line -notmatch '^AUTH (.*)$' -or ($Token -and $matches[1] -ne $Token)) {
        Write-Line $stream 'ERR auth'; Log ("session #{0} from {1}: BAD AUTH (rejected)" -f $sessionNum, $remote); continue
      }
      Write-Line $stream 'OK'
      while ($true) {
        $cmd = Read-Line $stream
        if ($null -eq $cmd) { break }
        if ($cmd -eq 'SYNC') {
          # Single pass = offer every file (size+mtime); the client fetches only
          # changed/new ones and deletes local files no longer offered (mirror).
          # With -Cutover a second pass runs after the operator stops the database.
          Log ("session #{0}: sync pass 1 - scanning {1}" -f $sessionNum, $Folder)
          $p1 = Send-Pass $stream $Folder
          if (-not $p1.Ok) { Log ("session #{0}: client dropped during pass 1" -f $sessionNum); break }
          Write-Line $stream 'PASS-END'
          Log ("session #{0}: pass 1 done - changed/new {1}, unchanged {2}, {3:N0} bytes" -f $sessionNum, $p1.Sent, $p1.Skipped, $p1.Bytes)
          if ($Cutover) {
            Log ("session #{0}: cutover - WAITING for you to stop the database and signal (keypress or ft-cutover.go)" -f $sessionNum)
            Wait-Cutover $stream
            Write-Line $stream 'GO'
            Log ("session #{0}: pass 2 (final, DB stopped) - scanning {1}" -f $sessionNum, $Folder)
            $p2 = Send-Pass $stream $Folder
            if (-not $p2.Ok) { Log ("session #{0}: client dropped during pass 2" -f $sessionNum); break }
            Write-Line $stream 'PASS-END'
            Log ("session #{0}: pass 2 done - changed/new {1}, unchanged {2}, {3:N0} bytes" -f $sessionNum, $p2.Sent, $p2.Skipped, $p2.Bytes)
          }
          Write-Line $stream 'DONE'
        }
        elseif ($cmd -eq 'BYE') { $completed = $true; break }
        else { Write-Line $stream 'ERR cmd' }
      }
    } catch { Log ("session #{0} from {1} ABORTED: {2}" -f $sessionNum, $remote, $_.Exception.Message) }
    finally { $client.Close() }
    $dur = [int]((Get-Date) - $t0).TotalSeconds
    if ($completed) { Log ("session #{0} completed cleanly in {1}s" -f $sessionNum, $dur) }
    else { Log ("session #{0} ended WITHOUT clean completion in {1}s - the client may reconnect to finish" -f $sessionNum, $dur) }
    if ($completed -and $Once) { Log 'one-time job done - shutting down'; return }
  }
} finally {
  $listener.Stop()
  Remove-FtCerts; Log 'TLS cert removed from store'
  if ($fwOpened) {
    Get-NetFirewallRule -DisplayName $fwRule -EA SilentlyContinue | Remove-NetFirewallRule -EA SilentlyContinue
    Log 'firewall rule removed (port closed)'
  }
}
