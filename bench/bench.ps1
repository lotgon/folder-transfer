# ====================================================================
#  folder-transfer - benchmark suite (run manually, commit BENCHMARKS.md)
#  Measures the things that matter and that we can reproduce:
#    A. small-file throughput + stream scaling, and the bare NTFS create floor
#    B. latency tolerance (throughput vs simulated RTT)
#    C. compression value vs channel speed (where packing pays and where it hurts)
#  Absolute MB/s are MACHINE-SPECIFIC (CPU, disk, antivirus). The conclusions
#  (compression pays below ~deflate speed; latency adds only a fixed cost; small
#  files are bound by file creation) are what transfer across machines.
#
#  Usage:   powershell -NoProfile -ExecutionPolicy Bypass -File bench\bench.ps1
#  Output:  BENCHMARKS.md at the repo root.
# ====================================================================
param(
  [int]$TinyCount = 10000,     # number of tiny files (small-files row + latency test)
  [int]$TinySizeKB = 4,
  [int]$LargeFileMB = 4,       # size of each large file (> 1 MB -> uses the adaptive Z path)
  # Large-corpus size scales with the channel so every run lasts ~10-15s and is not dominated by
  # fixed per-run overhead (handshakes, producer spin-up). One size per bandwidth column.
  [int]$MbSlow = 30,           # corpus MB for the 20 Mbit column
  [int]$MbMid = 150,           # corpus MB for the 100 Mbit column
  [int]$MbFast = 300,          # corpus MB for the 200 Mbit column and the LAN reference
  [int]$Port = 8722,           # server port; proxies listen on 9000+
  [string]$Out = '',
  [string]$WorkDir = '',       # scratch root for corpora/dest (default: <SystemDrive>\ft-bench)
  [switch]$KeepTmp             # keep the temp dir + per-run logs (debugging)
)
$script:rid = 0
$ErrorActionPreference = 'Stop'
$ScriptDir = $PSScriptRoot
$Repo = Split-Path $ScriptDir -Parent
$Server = Join-Path $Repo 'ft-server.ps1'
$Client = Join-Path $Repo 'ft-client.ps1'
$Proxy = Join-Path $ScriptDir 'bench-proxy.ps1'
if (-not $Out) { $Out = Join-Path $Repo 'BENCHMARKS.md' }
# Work in a clean root-level dir, NOT %TEMP%: AppData\Local\Temp often sits behind a filter
# driver (indexer/AV) that makes small-file creation several times slower and would dominate the
# small-files row. Override with -WorkDir if the system drive root is not writable.
$benchRoot = if ($WorkDir) { $WorkDir } else { Join-Path $env:SystemDrive '\ft-bench' }
$Tmp = Join-Path $benchRoot $PID
$TinySrc = Join-Path $Tmp 'tiny'
$IncS = Join-Path $Tmp 'inc_s'; $IncM = Join-Path $Tmp 'inc_m'; $IncL = Join-Path $Tmp 'inc_l'
$CompS = Join-Path $Tmp 'cmp_s'; $CompM = Join-Path $Tmp 'cmp_m'; $CompL = Join-Path $Tmp 'cmp_l'
$Dst = Join-Path $Tmp 'dst'
$DlDir = Join-Path $Repo 'download-scripts'
$logDir = Join-Path $Tmp 'logs'
[IO.Directory]::CreateDirectory($Tmp) | Out-Null
[IO.Directory]::CreateDirectory($logDir) | Out-Null
$inv = [Globalization.CultureInfo]::InvariantCulture

function Kill-Procs {
  Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" |
    Where-Object { $_.CommandLine -like '*ft-server.ps1*' -or $_.CommandLine -like '*bench-proxy.ps1*' } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}
function Wait-Port($p, $tries = 60) {
  for ($i = 0; $i -lt $tries; $i++) {
    try { (New-Object Net.Sockets.TcpClient).Connect('127.0.0.1', $p); return $true } catch { Start-Sleep -Milliseconds 300 }
  }
  return $false
}
function Parse-Bat($bat, $key) {
  $line = Select-String -Path $bat -Pattern ('^set "' + $key + '=') | Select-Object -First 1
  if (-not $line) { return '' }
  if ($line.Line -match ('^set "' + $key + '=([^"]*)"')) { return $matches[1] }
  return ''
}

# Run one transfer. Returns @{ Sec; MBps; Files; Ok }.
# $serverExtra is appended to the server command (e.g. '-NoCompress -Streams 4').
# If $rateMBps/$delayMs are set, traffic goes through the proxy on $listenPort.
function Run-Xfer($src, $serverExtra, $clientStreams, $delayMs, $rateMBps, $listenPort) {
  $rid = ++$script:rid
  Kill-Procs; Start-Sleep -Milliseconds 800
  if (Test-Path $Dst) { Remove-Item -Recurse -Force $Dst }
  if (Test-Path $DlDir) { Remove-Item -Recurse -Force $DlDir }
  $useProxy = ($delayMs -gt 0 -or $rateMBps -gt 0 -or $listenPort -gt 0)
  $connPort = $Port
  if ($useProxy) {
    $connPort = $listenPort
    Start-Process powershell -WindowStyle Hidden -ArgumentList @(
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Proxy,
      '-Listen', $listenPort, '-TargetHost', '127.0.0.1', '-TargetPort', $Port,
      '-DelayMs', $delayMs, '-RateMBps', ([double]$rateMBps).ToString($inv)) `
      -RedirectStandardOutput (Join-Path $logDir "proxy$rid.out") -RedirectStandardError (Join-Path $logDir "proxy$rid.err")
    if (-not (Wait-Port $listenPort 40)) { return @{ Ok = $false } }
  }
  # server
  $srvArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Server, $src,
    '-ServerHost', '127.0.0.1', '-Port', $Port, '-NoFirewall', '-IdleSeconds', '600',
    '-StallTimeout', '600', '-Streams', $clientStreams) + @($serverExtra -split ' ' | Where-Object { $_ -ne '' })
  Start-Process powershell -WindowStyle Hidden -ArgumentList $srvArgs `
    -RedirectStandardOutput (Join-Path $logDir "srv$rid.out") -RedirectStandardError (Join-Path $logDir "srv$rid.err")
  $bat = $null
  for ($i = 0; $i -lt 80; $i++) {
    $b = Get-ChildItem -Path $DlDir -Filter '*.bat' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($b) { $bat = $b.FullName; break }
    Start-Sleep -Milliseconds 300
  }
  if (-not $bat) { Kill-Procs; return @{ Ok = $false } }
  $token = Parse-Bat $bat 'TOKEN'
  $fp = Parse-Bat $bat 'FP'
  # client (synchronous)
  $cliLog = Join-Path $logDir "cli$rid.out"
  Start-Process powershell -WindowStyle Hidden -Wait -ArgumentList @(
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Client,
    '-Server', '127.0.0.1', '-Port', $connPort, '-Token', $token, '-Fingerprint', $fp,
    '-ToFolder', $Dst, '-Streams', $clientStreams) `
    -RedirectStandardOutput $cliLog -RedirectStandardError (Join-Path $logDir "cli$rid.err")
  Kill-Procs
  $txt = Get-Content -Raw $cliLog -ErrorAction SilentlyContinue
  if ($txt -match 'sync done\..*fetched=(\d+).*in (\d+:\d+:\d+) @ ([\d.]+) MB/s') {
    $files = [int]$matches[1]; $sec = [TimeSpan]::Parse($matches[2]).TotalSeconds; $mbps = [double]$matches[3]
    return @{ Ok = ($files -gt 0); Sec = $sec; MBps = $mbps; Files = $files }
  }
  return @{ Ok = $false }
}

function Fmt($r) { if ($r.Ok) { '{0:N1}s / {1:N1} MB/s' -f $r.Sec, $r.MBps } else { 'FAIL' } }
function FmtMB($r) { if ($r.Ok) { '{0:N1} MB/s' -f $r.MBps } else { 'FAIL' } }
# Efficiency = goodput / channel capacity. >100% means compression delivered more original
# data than the wire physically carried.
function Kpd($r, $cap) { if ($r.Ok -and $cap -gt 0) { '{0:N0}%' -f (100.0 * $r.MBps / $cap) } else { 'FAIL' } }

# -------------------- corpora --------------------
Write-Host 'Building corpora...'
$rnd = New-Object Random

# small files: many tiny files (create-bound on the receiver; sent in bundles, never compressed)
[IO.Directory]::CreateDirectory((Join-Path $TinySrc 'F')) | Out-Null
$tb = New-Object byte[] ($TinySizeKB * 1024); $rnd.NextBytes($tb)
for ($i = 0; $i -lt $TinyCount; $i++) { [IO.File]::WriteAllBytes((Join-Path $TinySrc ('F\f{0}.bin' -f $i)), $tb) }

# one ~LargeFileMB block of natural-ish text, reused for every compressible file
$words = ('the quick brown fox jumps over a lazy dog lorem ipsum dolor sit amet consectetur adipiscing ' +
  'elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis').Split(' ')
$sb = New-Object Text.StringBuilder
while ($sb.Length -lt ($LargeFileMB * 1MB)) { [void]$sb.Append($words[$rnd.Next($words.Length)]); [void]$sb.Append(' '); if ($rnd.Next(12) -eq 0) { [void]$sb.Append("`r`n") } }
$base = $sb.ToString()
# real deflate ratio of that block (the corpus ratio, since every file is ~this block)
$bb = [Text.Encoding]::UTF8.GetBytes($base)
$ms = New-Object IO.MemoryStream
$dz = New-Object IO.Compression.DeflateStream($ms, [IO.Compression.CompressionLevel]::Fastest, $true)
$dz.Write($bb, 0, $bb.Length); $dz.Close(); $ratio = [double]$bb.Length / $ms.Length; $ms.Dispose()

function Build-Comp($dir, $mb) {
  [IO.Directory]::CreateDirectory((Join-Path $dir 'F')) | Out-Null
  $n = [math]::Max(1, [int]($mb / $LargeFileMB))
  for ($i = 1; $i -le $n; $i++) { [IO.File]::WriteAllText((Join-Path $dir ('F\doc{0}.txt' -f $i)), ('file ' + $i + "`r`n" + $base)) }
}
function Build-Inc($dir, $mb) {
  [IO.Directory]::CreateDirectory((Join-Path $dir 'F')) | Out-Null
  $n = [math]::Max(1, [int]($mb / $LargeFileMB))
  $rb = New-Object byte[] ($LargeFileMB * 1MB)
  for ($i = 1; $i -le $n; $i++) { $rnd.NextBytes($rb); [IO.File]::WriteAllBytes((Join-Path $dir ('F\rnd{0}.bin' -f $i)), $rb) }
}
Build-Comp $CompS $MbSlow; Build-Comp $CompM $MbMid; Build-Comp $CompL $MbFast
Build-Inc $IncS $MbSlow; Build-Inc $IncM $MbMid; Build-Inc $IncL $MbFast

# bare file-creation floor (native, no network) - shows the wall small files hit
Add-Type -TypeDefinition @'
using System; using System.IO; using System.Threading.Tasks;
public class FloorProbe {
  public static double Create(string root, int n, int total, int sizeBytes) {
    if (Directory.Exists(root)) Directory.Delete(root, true);
    Directory.CreateDirectory(root);
    var dt = new DateTime(637000000000000000L, DateTimeKind.Utc);
    var sw = System.Diagnostics.Stopwatch.StartNew();
    int per = total / n;
    Parallel.For(0, n, k => {
      int from = k*per; int to = (k==n-1)? total : from+per;
      byte[] t = new byte[sizeBytes];
      for (int i=from;i<to;i++){
        string p = root + "\\f" + i + ".bin";
        using (var fs = new FileStream(p, FileMode.Create, FileAccess.Write)) { fs.Write(t,0,t.Length); }
        File.SetLastWriteTimeUtc(p, dt);
      }
    });
    sw.Stop(); Directory.Delete(root, true);
    return sw.Elapsed.TotalSeconds;
  }
}
'@
$floorRoot = Join-Path $Tmp 'floor'
$floor1 = [FloorProbe]::Create($floorRoot, 1, $TinyCount, $TinySizeKB * 1024)
$floor12 = [FloorProbe]::Create($floorRoot, 12, $TinyCount, $TinySizeKB * 1024)

# -------------------- main matrix: data type x (channel, ping) at default settings --------------------
# Default settings = 4 parallel streams + adaptive compression. Each channel is tested at 0 ms and at
# 150 ms RTT (delayMs = RTT/2 = 75). Cells become efficiency (goodput / channel capacity) in the
# report; here we just collect the goodput.
Write-Host 'Matrix: small files...'
$small_lan = Run-Xfer $TinySrc '' 4 0 0 0
$small_20 = Run-Xfer $TinySrc '' 4 0 2.5 9201
$small_20l = Run-Xfer $TinySrc '' 4 75 2.5 9202
$small_100 = Run-Xfer $TinySrc '' 4 0 12.5 9203
$small_100l = Run-Xfer $TinySrc '' 4 75 12.5 9204
$small_200 = Run-Xfer $TinySrc '' 4 0 25 9205
$small_200l = Run-Xfer $TinySrc '' 4 75 25 9206
Write-Host 'Matrix: large incompressible...'
$inc_lan = Run-Xfer $IncL '' 4 0 0 0
$inc_20 = Run-Xfer $IncS '' 4 0 2.5 9207
$inc_20l = Run-Xfer $IncS '' 4 75 2.5 9208
$inc_100 = Run-Xfer $IncM '' 4 0 12.5 9209
$inc_100l = Run-Xfer $IncM '' 4 75 12.5 9210
$inc_200 = Run-Xfer $IncL '' 4 0 25 9211
$inc_200l = Run-Xfer $IncL '' 4 75 25 9212
Write-Host 'Matrix: large compressible...'
$cmp_lan = Run-Xfer $CompL '' 4 0 0 0
$cmp_20 = Run-Xfer $CompS '' 4 0 2.5 9213
$cmp_20l = Run-Xfer $CompS '' 4 75 2.5 9214
$cmp_100 = Run-Xfer $CompM '' 4 0 12.5 9215
$cmp_100l = Run-Xfer $CompM '' 4 75 12.5 9216
$cmp_200 = Run-Xfer $CompL '' 4 0 25 9217
$cmp_200l = Run-Xfer $CompL '' 4 75 25 9218

# -------------------- machine info --------------------
$os = (Get-CimInstance Win32_OperatingSystem)
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1)
$ramGB = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB)
$defRt = 'unknown'
try { $defRt = [string](Get-MpComputerStatus).RealTimeProtectionEnabled } catch {}
$ver = 'unknown'
try { $ver = (& git -C $Repo describe --tags --abbrev=0 2>$null); if (-not $ver) { $ver = 'unknown' } } catch {}
$now = (Get-Date).ToString('yyyy-MM-dd HH:mm')

# -------------------- write BENCHMARKS.md --------------------
$nl = "`r`n"
$md = New-Object Text.StringBuilder
function L($s) { [void]$md.Append($s); [void]$md.Append("`r`n") }

L '# folder-transfer benchmarks'
L ''
L ('Generated: ' + $now + '  |  Version: ' + $ver)
L ''
L '> Efficiency (%) is largely transferable across machines; the underlying capacities and the'
L '> file-creation floor below are SPECIFIC TO THIS MACHINE (CPU, disk, antivirus). Regenerate'
L '> with `powershell -NoProfile -ExecutionPolicy Bypass -File bench\bench.ps1`.'
L ''
L '## Test machine'
L ''
L '| | |'
L '|---|---|'
L ('| OS | ' + $os.Caption + ' (' + $os.Version + ') |')
L ('| CPU | ' + $cpu.Name.Trim() + ' (' + [Environment]::ProcessorCount + ' logical) |')
L ('| RAM | ' + $ramGB + ' GB |')
L ('| Defender real-time | ' + $defRt + ' |')
L ''
L 'WAN links are emulated by a local proxy (`bench/bench-proxy.ps1`) that adds RTT and/or'
L 'caps bandwidth without coupling the two.'
L ''

L '## Efficiency by data type, channel and ping'
L ''
L 'Cells are EFFICIENCY = goodput / channel capacity - how much of the link we actually use.'
L 'About 100% means the link is saturated; ABOVE 100% means adaptive compression delivered more'
L 'original data than the wire physically carried; below 100% means we are bottlenecked off-link'
L '(small files = receiver disk). Each bandwidth is shown at 0 ms and at 150 ms round-trip. Default'
L 'settings (4 parallel streams + adaptive compression). Channel capacities: 20 Mbit = 2.5 MB/s,'
L '100 Mbit = 12.5 MB/s, 200 Mbit = 25 MB/s.'
L ''
L '| data type | 20 Mbit | 20 Mbit +150ms | 100 Mbit | 100 Mbit +150ms | 200 Mbit | 200 Mbit +150ms |'
L '|---|---|---|---|---|---|---|'
L ('| small files (' + $TinyCount + ' x ' + $TinySizeKB + ' KB) | ' + (Kpd $small_20 2.5) + ' | ' + (Kpd $small_20l 2.5) + ' | ' + (Kpd $small_100 12.5) + ' | ' + (Kpd $small_100l 12.5) + ' | ' + (Kpd $small_200 25) + ' | ' + (Kpd $small_200l 25) + ' |')
L ('| large, incompressible (' + $LargeFileMB + ' MB files, random) | ' + (Kpd $inc_20 2.5) + ' | ' + (Kpd $inc_20l 2.5) + ' | ' + (Kpd $inc_100 12.5) + ' | ' + (Kpd $inc_100l 12.5) + ' | ' + (Kpd $inc_200 25) + ' | ' + (Kpd $inc_200l 25) + ' |')
L ('| large, compressible (' + $LargeFileMB + ' MB files, text ' + ('{0:N2}x' -f $ratio) + ') | ' + (Kpd $cmp_20 2.5) + ' | ' + (Kpd $cmp_20l 2.5) + ' | ' + (Kpd $cmp_100 12.5) + ' | ' + (Kpd $cmp_100l 12.5) + ' | ' + (Kpd $cmp_200 25) + ' | ' + (Kpd $cmp_200l 25) + ' |')
L ''
L ('On LAN (loopback) the link is not the bottleneck, so link-efficiency is not meaningful. For reference, raw LAN goodput on this machine: small files ' + (FmtMB $small_lan) + ', incompressible ' + (FmtMB $inc_lan) + ', compressible ' + (FmtMB $cmp_lan) + '.')
L ''
L '**How to read it:**'
L '- *Incompressible* large files stay link-bound (high %): nothing to compress, so we push raw at close to the wire rate. The shortfall from 100% is per-bundle negotiation, which grows on faster links.'
L ('- *Compressible* large files go ABOVE 100% (data packs ~' + ('{0:N1}x' -f $ratio) + '): adaptive compression sends fewer bytes, so more original data arrives per second than the wire could carry raw. On a fast LAN it would stay near raw - compressing there only costs CPU.')
L ('- *Small files* stay below 100%: they are bound by file creation on the receiver (NTFS metadata + antivirus), not the link. Bare create floor on this disk: ' + ('{0:N1}s' -f $floor1) + ' single-thread / ' + ('{0:N1}s' -f $floor12) + ' parallel for ' + $TinyCount + ' files.')
L '- *Latency* costs little: one round-trip per ~10 MB bundle, not per file. The +150ms columns stay close to 0 ms; the gap only widens on the fastest links, where each transfer is so short that the few fixed round-trips are a visible slice of it.'
L ''

[IO.File]::WriteAllText($Out, $md.ToString(), (New-Object Text.UTF8Encoding($false)))
Kill-Procs
if ($KeepTmp) { Write-Host ('Temp kept at ' + $Tmp) }
elseif (Test-Path $Tmp) { Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue }
Write-Host ('Wrote ' + $Out)
