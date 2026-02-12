//! Stress test for seedlink-rs server.
//!
//! Measures push throughput, fan-out delivery, and concurrent client handling.
//!
//! ```bash
//! # Debug mode (correctness check)
//! cargo run --example stress_test -p seedlink-rs-server
//!
//! # Release mode (meaningful throughput numbers)
//! cargo run --example stress_test -p seedlink-rs-server --release
//!
//! # High load
//! CLIENTS=200 RECORDS=50000 cargo run --example stress_test -p seedlink-rs-server --release
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use seedlink_rs_client::{ClientConfig, SeedLinkClient};
use seedlink_rs_protocol::frame::v3;
use seedlink_rs_server::{SeedLinkServer, ServerConfig};
use tokio::sync::Barrier;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.replace('_', "").parse().ok())
        .unwrap_or(default)
}

/// Build a 512-byte miniSEED-like payload with station/network in header.
fn make_payload(station: &str, network: &str) -> Vec<u8> {
    let mut payload = vec![0u8; v3::PAYLOAD_LEN];
    let sta_bytes = station.as_bytes();
    for (i, &b) in sta_bytes.iter().enumerate().take(5) {
        payload[8 + i] = b;
    }
    for i in sta_bytes.len()..5 {
        payload[8 + i] = b' ';
    }
    let net_bytes = network.as_bytes();
    for (i, &b) in net_bytes.iter().enumerate().take(2) {
        payload[18 + i] = b;
    }
    for i in net_bytes.len()..2 {
        payload[18 + i] = b' ';
    }
    payload
}

#[tokio::main]
async fn main() {
    let num_clients = env_or("CLIENTS", 50) as usize;
    let num_records = env_or("RECORDS", 10_000) as usize;
    let ring_cap = env_or("RING_CAP", 20_000) as usize;

    println!("seedlink-rs stress test");
    println!("========================");

    // Phase 1: Start server
    let config = ServerConfig {
        ring_capacity: ring_cap,
        ..ServerConfig::default()
    };
    let server = match SeedLinkServer::bind_with_config("127.0.0.1:0", config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: failed to bind server: {e}");
            std::process::exit(1);
        }
    };
    let addr = server.local_addr().unwrap().to_string();
    let store = server.store().clone();
    let shutdown = server.shutdown_handle();
    tokio::spawn(server.run());
    tokio::task::yield_now().await;

    println!("Server:  {addr} (ring_capacity={ring_cap})");
    println!("Clients: {num_clients}");
    println!("Records: {num_records}");
    println!();

    // Phase 2: Spawn client tasks
    let total_received = Arc::new(AtomicU64::new(0));
    // barrier: N clients + 1 pusher
    let barrier = Arc::new(Barrier::new(num_clients + 1));
    let mut per_client_counts: Vec<Arc<AtomicU64>> = Vec::with_capacity(num_clients);
    let mut handles = Vec::with_capacity(num_clients);

    let connect_start = Instant::now();

    for i in 0..num_clients {
        let addr = addr.clone();
        let barrier = barrier.clone();
        let total_received = total_received.clone();
        let per_count = Arc::new(AtomicU64::new(0));
        per_client_counts.push(per_count.clone());
        let expected = num_records as u64;

        handles.push(tokio::spawn(async move {
            // Connect with v3 (simpler frames, no negotiation overhead)
            let config = ClientConfig {
                prefer_v4: false,
                read_timeout: std::time::Duration::from_secs(60),
                ..ClientConfig::default()
            };
            let mut client = match SeedLinkClient::connect_with_config(&addr, config).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  client {i}: connect failed: {e}");
                    barrier.wait().await;
                    return 0u64;
                }
            };

            if let Err(e) = client.station("ANMO", "IU").await {
                eprintln!("  client {i}: STATION failed: {e}");
                barrier.wait().await;
                return 0;
            }
            if let Err(e) = client.data().await {
                eprintln!("  client {i}: DATA failed: {e}");
                barrier.wait().await;
                return 0;
            }
            if let Err(e) = client.end_stream().await {
                eprintln!("  client {i}: END failed: {e}");
                barrier.wait().await;
                return 0;
            }

            // Signal ready
            barrier.wait().await;

            // Receive frames
            let mut count = 0u64;
            while count < expected {
                match client.next_frame().await {
                    Ok(Some(_)) => {
                        count += 1;
                        per_count.fetch_add(1, Ordering::Relaxed);
                        total_received.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(None) => break, // EOF
                    Err(_) => break,   // error
                }
            }
            count
        }));
    }

    let connect_elapsed = connect_start.elapsed();
    println!(
        "Connecting {num_clients} clients... done ({:.0?})",
        connect_elapsed
    );

    // Phase 3: Wait for all clients to be ready, then push
    barrier.wait().await;

    let push_start = Instant::now();
    let payload = make_payload("ANMO", "IU");
    for _ in 0..num_records {
        store.push("IU", "ANMO", &payload);
    }
    let push_elapsed = push_start.elapsed();
    println!(
        "Pushing {num_records} records... done ({:.0?})",
        push_elapsed
    );

    // Phase 4: Wait for delivery (with timeout)
    let expected_total = (num_clients as u64) * (num_records as u64);
    let wait_start = Instant::now();

    let delivery_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        for h in handles {
            let _ = h.await;
        }
    })
    .await;

    let wait_elapsed = wait_start.elapsed();
    let timed_out = delivery_result.is_err();

    if timed_out {
        println!("Waiting for delivery... TIMEOUT after {:.0?}", wait_elapsed);
    } else {
        println!("Waiting for delivery... done ({:.0?})", wait_elapsed);
    }

    // Phase 5: Shutdown and print results
    shutdown.shutdown();

    let actual_total = total_received.load(Ordering::Relaxed);
    let wall_clock = connect_elapsed + push_elapsed + wait_elapsed;
    let push_rate = if push_elapsed.as_secs_f64() > 0.0 {
        num_records as f64 / push_elapsed.as_secs_f64()
    } else {
        f64::INFINITY
    };
    let recv_rate = if wait_elapsed.as_secs_f64() > 0.0 {
        actual_total as f64 / wait_elapsed.as_secs_f64()
    } else {
        f64::INFINITY
    };

    // Per-client stats
    let counts: Vec<u64> = per_client_counts
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();
    let min = counts.iter().copied().min().unwrap_or(0);
    let max = counts.iter().copied().max().unwrap_or(0);
    let avg = if counts.is_empty() {
        0
    } else {
        actual_total / counts.len() as u64
    };

    println!();
    println!("Results");
    println!("-------");
    println!(
        "Total frames delivered: {actual_total} ({num_clients} clients x {num_records} records)"
    );
    println!("Push throughput:        {:.0} records/sec", push_rate);
    println!("Receive throughput:     {:.0} frames/sec", recv_rate);
    println!("Wall clock:             {:.2?}", wall_clock);
    println!();
    println!("Per-client: min={min}  max={max}  avg={avg}");

    if actual_total == expected_total && !timed_out {
        println!("All clients received all records: OK");
    } else if timed_out {
        println!(
            "WARNING: timeout — delivered {actual_total}/{expected_total} ({:.1}%)",
            actual_total as f64 / expected_total as f64 * 100.0
        );
    } else {
        println!(
            "MISMATCH: expected {expected_total}, got {actual_total} ({:.1}%)",
            actual_total as f64 / expected_total as f64 * 100.0
        );
    }
}
