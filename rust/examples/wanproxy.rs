//! Test-only WAN-emulation TCP proxy (a faster Rust replacement for bench/proxy.py).
//!
//! Adds a fixed one-way delay (RTT = 2 * delay) and, if a rate is set, caps
//! AGGREGATE throughput with a single GLOBAL rate gate shared across all
//! connections and both directions — a real WAN link is one shared pipe, so N
//! parallel streams must share the bandwidth, not get N x it. In-flight is bounded
//! to the bandwidth-delay product so the sender feels real backpressure.
//!
//! Unlike the Python version this is not GIL-bound: real OS threads pump each
//! direction, so it stays accurate at high rates with many parallel streams (the
//! Python proxy under-delivered at e.g. 200 Mbit x 4+ streams, skewing benchmarks).
//!
//! Build:  cargo build --release --example wanproxy
//! Run:    target/release/examples/wanproxy --listen 9000 --target-port 8722 \
//!             --delay-ms 75 --rate-mbps 25   [--target-host 127.0.0.1]
//!
//! It is an `example` on purpose: it is never compiled into the released `ft` binary.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CHUNK: usize = 65536;

/// Total bytes delivered in the measured direction (set per pump), for ratio math.
static WIRE: AtomicU64 = AtomicU64::new(0);

/// The shared emulated link: a global rate gate plus a one-way delay.
struct Link {
    rate: f64,    // bytes/sec for the whole link; 0 = unlimited
    delay: f64,   // one-way delay, seconds
    start: Instant,
    /// Wall-clock (seconds since `start`) the link is reserved until — the gate.
    busy_until: Mutex<f64>,
}

impl Link {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Reserve `len` bytes of link time and return when that send finishes
    /// (seconds since `start`). Serializes all callers through one shared clock,
    /// so 4 streams share the bandwidth exactly like one physical pipe.
    fn reserve(&self, len: usize) -> f64 {
        let mut bu = self.busy_until.lock().unwrap();
        let now = self.now();
        let start = if now > *bu { now } else { *bu };
        let finish = start + len as f64 / self.rate;
        *bu = finish;
        finish
    }
}

/// Pump one direction: read from `src`, hold each chunk for the one-way delay,
/// pace it through the global rate gate, then write to `dst`. When `measure`, the
/// bytes are added to the global WIRE counter (used to compute the realized ratio).
fn pump(mut src: TcpStream, mut dst: TcpStream, link: Arc<Link>, measure: bool) {
    // In-flight buffer ~ BDP (bytes-in-flight a real link of this rate*RTT holds),
    // so when the writer is pacing, the reader blocks and `src`'s TCP window fills.
    let cap = if link.rate > 0.0 {
        ((link.rate * link.delay * 2.0 / CHUNK as f64) as usize) + 4
    } else {
        1 << 14 // effectively unbounded with no rate cap
    };
    let (tx, rx) = sync_channel::<(Vec<u8>, f64)>(cap.max(4));

    let lw = link.clone();
    let writer = thread::spawn(move || {
        while let Ok((chunk, release_at)) = rx.recv() {
            // One-way propagation delay: don't deliver before its release time.
            let wait = release_at - lw.now();
            if wait > 0.0 {
                thread::sleep(Duration::from_secs_f64(wait));
            }
            // Shared bandwidth gate.
            if lw.rate > 0.0 {
                let finish = lw.reserve(chunk.len());
                let ahead = finish - lw.now();
                if ahead >= 0.001 {
                    thread::sleep(Duration::from_secs_f64(ahead));
                }
            }
            if dst.write_all(&chunk).is_err() {
                break;
            }
        }
        let _ = dst.shutdown(Shutdown::Write); // half-close; reverse direction lives on
    });

    let mut buf = vec![0u8; CHUNK];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if measure {
                    WIRE.fetch_add(n as u64, Ordering::Relaxed);
                }
                let release_at = link.now() + link.delay;
                if tx.send((buf[..n].to_vec(), release_at)).is_err() {
                    break;
                }
            }
        }
    }
    drop(tx); // channel closed -> writer drains the queue and half-closes dst
    let _ = writer.join();
    if measure {
        eprintln!("[wanproxy] wire_down_total={}", WIRE.load(Ordering::Relaxed));
    }
}

fn handle(client: TcpStream, target: String, link: Arc<Link>) {
    let server = match TcpStream::connect(&target) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = server.set_nodelay(true);
    let _ = client.set_nodelay(true);
    let (c2, s2) = match (client.try_clone(), server.try_clone()) {
        (Ok(c), Ok(s)) => (c, s),
        _ => return,
    };
    let up = {
        let link = link.clone();
        thread::spawn(move || pump(c2, s2, link, false)) // client -> server (control)
    };
    pump(server, client, link, true); // server -> client (the measured download)
    let _ = up.join();
}

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen: u16 = arg(&args, "--listen").and_then(|s| s.parse().ok()).expect("--listen <port> required");
    let tport: u16 = arg(&args, "--target-port").and_then(|s| s.parse().ok()).expect("--target-port <port> required");
    let host = arg(&args, "--target-host").unwrap_or_else(|| "127.0.0.1".to_string());
    let delay_ms: u64 = arg(&args, "--delay-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let rate_mbps: f64 = arg(&args, "--rate-mbps").and_then(|s| s.parse().ok()).unwrap_or(0.0);

    let link = Arc::new(Link {
        rate: rate_mbps * 1024.0 * 1024.0,
        delay: delay_ms as f64 / 1000.0,
        start: Instant::now(),
        busy_until: Mutex::new(0.0),
    });

    let listener = TcpListener::bind(("127.0.0.1", listen)).expect("bind");
    let rate = if rate_mbps > 0.0 { format!("{rate_mbps} MB/s") } else { "unlimited".to_string() };
    // Same startup line the harness greps for ("proxy 127").
    println!("proxy 127.0.0.1:{listen} -> {host}:{tport}  RTT {}ms  rate {rate}", 2 * delay_ms);
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    let target = format!("{host}:{tport}");
    for client in listener.incoming().flatten() {
        let link = link.clone();
        let target = target.clone();
        thread::spawn(move || handle(client, target, link));
    }
}
