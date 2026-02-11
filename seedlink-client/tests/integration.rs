//! Integration tests that connect to real SeedLink servers.
//!
//! These tests are gated by environment variables:
//! - `SEEDLINK_TEST_SERVER` — v3 server (e.g., `rtserve.iris.washington.edu:18000`)
//! - `SEEDLINK_V4_TEST_SERVER` — v4 server (e.g., `localhost:18000`)

use std::time::Duration;

use seedlink_client::{ClientConfig, SeedLinkClient};
use seedlink_protocol::{InfoLevel, ProtocolVersion};

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
    assert_eq!(client.state(), seedlink_client::ClientState::Disconnected);
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
