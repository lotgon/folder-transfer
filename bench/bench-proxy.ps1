param([int]$Listen, [string]$TargetHost = '127.0.0.1', [int]$TargetPort, [int]$DelayMs = 0, [double]$RateMBps = 0)
# Test-only TCP proxy used by bench.ps1 to emulate a WAN link between client and server.
# Adds a fixed ONE-WAY delay of $DelayMs to each direction (so RTT = 2*DelayMs) and, if
# $RateMBps > 0, caps each direction's throughput to that rate. Bandwidth and latency are
# decoupled: a reader thread fills a queue continuously; a writer thread releases each chunk
# only after its delay has elapsed AND only as fast as the rate allows. On EOF it half-closes
# the destination's send side (not the whole socket) so the reverse direction keeps flowing -
# essential because the classic client half-closes after its request and then only receives.
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Collections.Concurrent;
public class LatProxy {
  static int Delay;
  static double Rate;   // bytes/sec for the whole link (shared across ALL connections); 0 = unlimited
  // Global, shared rate gate: a real WAN link is one pipe shared by every parallel stream, so the
  // throttle must be aggregate, not per-connection (4 streams must NOT get 4x the bandwidth).
  static readonly object RLock = new object();
  static double RBusyUntil = 0;
  static System.Diagnostics.Stopwatch RSw = System.Diagnostics.Stopwatch.StartNew();
  static void Pump(NetworkStream src, NetworkStream dst, Socket dstSock) {
    // In-flight buffer size. When throttling we allow the bandwidth-delay product (BDP) in flight,
    // exactly like real TCP would: too small a buffer would artificially throttle a high-BDP link
    // (e.g. 200 Mbit x 150 ms). At 0 ms RTT the BDP is ~0, so this collapses to a tiny buffer that
    // still forces real backpressure for the rate measurement. No rate cap (pure latency test) ->
    // effectively unbounded.
    int cap = (1<<20);
    if (Rate > 0) {
      long bdp = (long)(Rate * (2.0 * Delay / 1000.0));   // bytes = bytes/s * RTT(s)
      cap = (int)Math.Max(4, bdp / 65536 + 4);
    }
    var q = new BlockingCollection<Tuple<byte[],long>>(cap);
    var writer = new Thread(() => {
      try {
        foreach (var it in q.GetConsumingEnumerable()) {
          long waitMs = (it.Item2 - DateTime.UtcNow.Ticks) / 10000;
          if (waitMs > 0) Thread.Sleep((int)waitMs);
          if (Rate > 0) {
            // reserve this chunk's slot on the shared link timeline
            double finish;
            lock (RLock) {
              double now = RSw.Elapsed.TotalSeconds;
              double start = now > RBusyUntil ? now : RBusyUntil;
              finish = start + it.Item1.Length / Rate;
              RBusyUntil = finish;
            }
            // Thread.Sleep granularity is ~15 ms, so only sleep once we are at least that far ahead
            // of the timeline; the accumulating RBusyUntil keeps the AVERAGE rate accurate even
            // though individual sleeps are coarse. (Sleeping per chunk would overshoot and throttle
            // high-bandwidth links far below their nominal rate.)
            double aheadMs = (finish - RSw.Elapsed.TotalSeconds) * 1000.0;
            if (aheadMs >= 15) Thread.Sleep((int)aheadMs);
          }
          dst.Write(it.Item1, 0, it.Item1.Length); dst.Flush();
        }
      } catch {} finally { try { dstSock.Shutdown(SocketShutdown.Send); } catch {} }
    });
    writer.IsBackground = true; writer.Start();
    var buf = new byte[65536];
    try {
      int n;
      while ((n = src.Read(buf, 0, buf.Length)) > 0) {
        var c = new byte[n]; Array.Copy(buf, c, n);
        q.Add(Tuple.Create(c, DateTime.UtcNow.AddMilliseconds(Delay).Ticks));
      }
    } catch {} finally { q.CompleteAdding(); }
    writer.Join();
  }
  public static void Start(int listenPort, string th, int tp, int delayMs, double rateBytesPerSec) {
    Delay = delayMs; Rate = rateBytesPerSec;
    var l = new TcpListener(IPAddress.Loopback, listenPort); l.Start();
    while (true) {
      var c = l.AcceptTcpClient(); c.NoDelay = true;
      var t = new Thread(() => {
        TcpClient s = null;
        try {
          s = new TcpClient(); s.NoDelay = true; s.Connect(th, tp);
          var cs = c.GetStream(); var ss = s.GetStream();
          var up = new Thread(() => Pump(cs, ss, s.Client)); up.IsBackground = true; up.Start();
          Pump(ss, cs, c.Client);
          up.Join();
        } catch {} finally { try { c.Close(); } catch {} try { if (s != null) s.Close(); } catch {} }
      });
      t.IsBackground = true; t.Start();
    }
  }
}
'@
$rateBps = [double]$RateMBps * 1MB
Write-Host ("proxy: 127.0.0.1:{0} -> {1}:{2}  RTT {3}ms  rate {4}" -f $Listen, $TargetHost, $TargetPort, (2 * $DelayMs), $(if ($RateMBps -gt 0) { "$RateMBps MB/s" } else { 'unlimited' }))
[LatProxy]::Start($Listen, $TargetHost, $TargetPort, $DelayMs, $rateBps)
