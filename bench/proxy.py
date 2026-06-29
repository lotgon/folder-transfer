#!/usr/bin/env python3
"""Test-only TCP WAN-emulation proxy for the Rust benchmark.

A faithful port of bench/bench-proxy.ps1: adds a fixed one-way delay (RTT =
2*delay) and, if a rate is set, caps aggregate throughput with a single GLOBAL
rate gate shared across all connections (a real WAN link is one shared pipe, so
4 parallel streams must not get 4x the bandwidth). Bandwidth and latency are
decoupled via a per-direction delay queue, the in-flight buffer is bounded to the
bandwidth-delay product so the sender feels real backpressure, and EOF half-closes
only the destination's send side so the reverse direction keeps flowing.
"""
import argparse
import socket
import threading
import time
import queue

# Global shared rate gate (bytes/sec for the whole link; 0 = unlimited).
RATE = 0.0
DELAY = 0.0  # one-way delay, seconds
_rlock = threading.Lock()
_rbusy_until = 0.0
_start = time.monotonic()


def _now():
    return time.monotonic() - _start


def pump(src, dst):
    # In-flight buffer: BDP when throttling, tiny at 0ms RTT (still real backpressure),
    # effectively unbounded with no rate cap.
    if RATE > 0:
        bdp = RATE * (2.0 * DELAY)
        cap = max(4, int(bdp / 65536) + 4)
    else:
        cap = 1 << 20
    q = queue.Queue(maxsize=cap)

    def writer():
        global _rbusy_until
        while True:
            item = q.get()
            if item is None:
                break
            chunk, release_at = item
            wait = release_at - _now()
            if wait > 0:
                time.sleep(wait)
            if RATE > 0:
                with _rlock:
                    now = _now()
                    start = now if now > _rbusy_until else _rbusy_until
                    finish = start + len(chunk) / RATE
                    _rbusy_until = finish
                ahead = (finish - _now())
                if ahead >= 0.015:  # coarse sleep granularity, like the PS proxy
                    time.sleep(ahead)
            try:
                dst.sendall(chunk)
            except OSError:
                break
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass

    wt = threading.Thread(target=writer, daemon=True)
    wt.start()
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            q.put((data, _now() + DELAY))
    except OSError:
        pass
    finally:
        q.put(None)
    wt.join()


def handle(client, target_host, target_port):
    try:
        server = socket.create_connection((target_host, target_port))
        server.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        client.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        up = threading.Thread(target=pump, args=(client, server), daemon=True)
        up.start()
        pump(server, client)
        up.join()
    except OSError:
        pass
    finally:
        for s in (client,):
            try:
                s.close()
            except OSError:
                pass


def main():
    global RATE, DELAY
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", type=int, required=True)
    ap.add_argument("--target-port", type=int, required=True)
    ap.add_argument("--target-host", default="127.0.0.1")
    ap.add_argument("--delay-ms", type=int, default=0)
    ap.add_argument("--rate-mbps", type=float, default=0.0)
    a = ap.parse_args()
    DELAY = a.delay_ms / 1000.0
    RATE = a.rate_mbps * 1024 * 1024
    ls = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", a.listen))
    ls.listen(64)
    rate = f"{a.rate_mbps} MB/s" if a.rate_mbps > 0 else "unlimited"
    print(f"proxy 127.0.0.1:{a.listen} -> {a.target_host}:{a.target_port}  RTT {2*a.delay_ms}ms  rate {rate}", flush=True)
    while True:
        c, _ = ls.accept()
        threading.Thread(target=handle, args=(c, a.target_host, a.target_port), daemon=True).start()


if __name__ == "__main__":
    main()
