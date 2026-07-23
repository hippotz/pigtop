//! Standalone validation spike (see the project plan): proves out the ntstat kernel-control
//! protocol end-to-end — unprivileged socket connect, subscribe, poll, per-pid rate — before any
//! menu bar UI is built on top of it. Run with `cargo run --bin ntstat_spike` as a normal user,
//! then generate some traffic (e.g. `curl -o /dev/null https://speed.hetzner.de/100MB.bin`) and
//! confirm the responsible process rises to the top of the printed table.

use std::thread::sleep;
use std::time::Duration;

use pigtop::ntstat::NtstatClient;
use pigtop::rates::RateTracker;

fn main() {
    println!("connecting to com.apple.network.statistics as uid={} ...", unsafe {
        libc::getuid()
    });

    let mut client = match NtstatClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAILED to connect (this would be the privilege-assumption blocker): {e}");
            std::process::exit(1);
        }
    };
    println!("connected OK (no sudo used) — privilege assumption holds.\n");

    if let Err(e) = client.subscribe_tcp_udp() {
        eprintln!("FAILED to subscribe to TCP/UDP sources: {e}");
        std::process::exit(1);
    }
    println!("subscribed to TCP_KERNEL/TCP_USERLAND/UDP_KERNEL/UDP_USERLAND sources.\n");

    let mut tracker = RateTracker::new();

    for i in 0..20 {
        let poll_started = std::time::Instant::now();
        let samples = match client.poll_all() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("poll_all() failed: {e}");
                std::process::exit(1);
            }
        };
        let poll_elapsed = poll_started.elapsed();

        tracker.record(&samples);
        let ranked = tracker.snapshot();

        println!(
            "--- poll {i}: {} raw sources, {} distinct pids, poll took {:?} ---",
            samples.len(),
            ranked.len(),
            poll_elapsed
        );
        for p in ranked.iter().filter(|p| p.total_rate() > 0.0 || p.peak_rate > 0.0).take(8) {
            println!(
                "  pid={:<8} {:<28} rx={:>9.1} KB/s  tx={:>9.1} KB/s  peak={:>9.1} KB/s ({}s ago)",
                p.pid,
                p.name,
                p.rx_rate / 1024.0,
                p.tx_rate / 1024.0,
                p.peak_rate / 1024.0,
                p.peak_age.as_secs()
            );
        }

        sleep(Duration::from_millis(100));
    }
}
