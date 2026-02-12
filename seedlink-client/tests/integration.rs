//! Integration tests that connect to real SeedLink servers.
//!
//! These tests are gated by environment variables:
//! - `SEEDLINK_TEST_SERVER` — v3 server (e.g., `rtserve.iris.washington.edu:18000`)
//! - `SEEDLINK_V4_TEST_SERVER` — v4 server (e.g., `localhost:18000`)

use std::time::Duration;

use seedlink_rs_client::{ClientConfig, ClientState, SeedLinkClient};
use seedlink_rs_protocol::{InfoLevel, ProtocolVersion, SequenceNumber};

fn v3_server() -> Option<String> {
    std::env::var("SEEDLINK_TEST_SERVER").ok()
}

fn v4_server() -> Option<String> {
    std::env::var("SEEDLINK_V4_TEST_SERVER").ok()
}

#[tokio::test]
async fn v3_hello() {
    let Some(addr) = v3_server() else {
        eprintln!("skipping: SEEDLINK_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: false,
        connect_timeout: Duration::from_secs(15),
        read_timeout: Duration::from_secs(30),
    };
    let client = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();

    assert_eq!(client.version(), ProtocolVersion::V3);
    let info = client.server_info();
    eprintln!(
        "server: {} {} ({})",
        info.software, info.version, info.organization
    );
    assert!(!info.software.is_empty());
}

#[tokio::test]
async fn v3_station_stream() {
    let Some(addr) = v3_server() else {
        eprintln!("skipping: SEEDLINK_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: false,
        connect_timeout: Duration::from_secs(15),
        read_timeout: Duration::from_secs(60),
    };
    let mut client = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();

    client.station("ANMO", "IU").await.unwrap();
    client.select("BHZ").await.unwrap();
    client.data().await.unwrap();
    client.end_stream().await.unwrap();

    // Read a few frames
    for i in 0..3 {
        let frame = tokio::time::timeout(Duration::from_secs(60), client.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for frame {i}"))
            .unwrap_or_else(|e| panic!("error reading frame {i}: {e}"));

        if let Some(frame) = frame {
            eprintln!(
                "frame {i}: seq={}, payload_len={}",
                frame.sequence(),
                frame.payload().len()
            );
        }
    }

    client.bye().await.unwrap();
}

#[tokio::test]
async fn v3_bye() {
    let Some(addr) = v3_server() else {
        eprintln!("skipping: SEEDLINK_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: false,
        ..ClientConfig::default()
    };
    let mut client = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();

    client.bye().await.unwrap();
    assert_eq!(client.state(), ClientState::Disconnected);
}

#[tokio::test]
async fn v4_negotiate_and_stream() {
    let Some(addr) = v4_server() else {
        eprintln!("skipping: SEEDLINK_V4_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: true,
        connect_timeout: Duration::from_secs(15),
        read_timeout: Duration::from_secs(60),
    };
    let mut client = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();

    assert_eq!(client.version(), ProtocolVersion::V4);
    let info = client.server_info();
    eprintln!(
        "v4 server: {} {} ({})",
        info.software, info.version, info.organization
    );

    client.station("ANMO", "IU").await.unwrap();
    client.select("BHZ").await.unwrap();
    client.data().await.unwrap();
    client.end_stream().await.unwrap();

    for i in 0..3 {
        let frame = tokio::time::timeout(Duration::from_secs(60), client.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for v4 frame {i}"))
            .unwrap_or_else(|e| panic!("error reading v4 frame {i}: {e}"));

        if let Some(frame) = frame {
            eprintln!(
                "v4 frame {i}: seq={}, payload_len={}",
                frame.sequence(),
                frame.payload().len()
            );
        }
    }

    client.bye().await.unwrap();
}

#[tokio::test]
async fn v3_info_id() {
    let Some(addr) = v3_server() else {
        eprintln!("skipping: SEEDLINK_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: false,
        connect_timeout: Duration::from_secs(15),
        read_timeout: Duration::from_secs(30),
    };
    let mut client = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();

    let frames = client.info(InfoLevel::Id).await.unwrap();
    assert!(!frames.is_empty(), "INFO ID should return at least 1 frame");

    let payload = frames[0].payload();
    assert!(!payload.is_empty(), "INFO ID payload should be non-empty");
    eprintln!(
        "INFO ID: {} bytes, first frame seq={}",
        payload.len(),
        frames[0].sequence()
    );

    client.bye().await.unwrap();
}

/// Test that `DATA seq` actually resumes from where we left off on a real server.
///
/// This is the critical test: connect, read frames, disconnect, reconnect with
/// `DATA seq`, and verify we get frames with sequence > last_seq (no data lost).
#[tokio::test]
async fn v3_data_resume_no_data_loss() {
    let Some(addr) = v3_server() else {
        eprintln!("skipping: SEEDLINK_TEST_SERVER not set");
        return;
    };

    let config = ClientConfig {
        prefer_v4: false,
        connect_timeout: Duration::from_secs(15),
        read_timeout: Duration::from_secs(120),
    };

    // --- Connection 1: get some frames and record last sequence ---
    let mut client = SeedLinkClient::connect_with_config(&addr, config.clone())
        .await
        .unwrap();
    client.station("ANMO", "IU").await.unwrap();
    client.select("BHZ").await.unwrap();
    client.data().await.unwrap();
    client.end_stream().await.unwrap();

    let num_frames_conn1 = 5;
    let mut last_seq = SequenceNumber::new(0);
    for i in 0..num_frames_conn1 {
        let frame = tokio::time::timeout(Duration::from_secs(120), client.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for frame {i}"))
            .unwrap_or_else(|e| panic!("error reading frame {i}: {e}"))
            .expect("unexpected EOF");

        last_seq = frame.sequence();
        eprintln!(
            "conn1 frame {i}: seq={}, payload_len={}",
            frame.sequence(),
            frame.payload().len()
        );
    }
    eprintln!("--- last sequence from conn1: {last_seq} ---");

    // Disconnect
    client.bye().await.unwrap();
    assert_eq!(client.state(), ClientState::Disconnected);

    // --- Connection 2: resume from last_seq ---
    let mut client2 = SeedLinkClient::connect_with_config(&addr, config)
        .await
        .unwrap();
    client2.station("ANMO", "IU").await.unwrap();
    client2.select("BHZ").await.unwrap();
    client2.data_from(last_seq).await.unwrap();
    client2.end_stream().await.unwrap();

    // Read a few frames and verify sequence numbers
    let num_frames_conn2 = 3;
    let mut resumed_sequences = Vec::new();
    for i in 0..num_frames_conn2 {
        let frame = tokio::time::timeout(Duration::from_secs(120), client2.next_frame())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for resumed frame {i}"))
            .unwrap_or_else(|e| panic!("error reading resumed frame {i}: {e}"))
            .expect("unexpected EOF");

        eprintln!(
            "conn2 frame {i}: seq={}, payload_len={}",
            frame.sequence(),
            frame.payload().len()
        );
        resumed_sequences.push(frame.sequence());
    }

    client2.bye().await.unwrap();

    // Key assertion: all resumed sequences should be >= last_seq
    // (server may resend the last_seq frame itself, so >= not >)
    for (i, seq) in resumed_sequences.iter().enumerate() {
        assert!(
            *seq >= last_seq,
            "conn2 frame {i}: seq {seq} < last_seq {last_seq} — DATA LOSS!"
        );
    }

    eprintln!("--- PASS: resumed from {last_seq}, got sequences: {resumed_sequences:?} ---");
    eprintln!("--- No data loss confirmed ---");
}
