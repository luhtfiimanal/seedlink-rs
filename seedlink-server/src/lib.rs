//! Async SeedLink server for real-time seismic data distribution.
//!
//! Accept client connections and distribute miniSEED records
//! from configured data sources via an in-memory ring buffer.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> seedlink_rs_server::Result<()> {
//! use seedlink_rs_server::SeedLinkServer;
//!
//! let server = SeedLinkServer::bind("0.0.0.0:18000").await?;
//! let store = server.store().clone();
//!
//! tokio::spawn(server.run());
//!
//! // Push data from any source
//! let payload = vec![0u8; 512];
//! store.push("IU", "ANMO", &payload);
//! # Ok(())
//! # }
//! ```

pub mod error;
pub(crate) mod handler;
pub mod store;

pub use error::{Result, ServerError};
pub use store::DataStore;

use std::net::SocketAddr;

use handler::{ClientHandler, HandlerConfig};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

/// Configuration for [`SeedLinkServer`].
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Software name reported in HELLO response. Default: `"SeedLink"`.
    pub software: String,
    /// Version string reported in HELLO response. Default: `"v3.1"`.
    pub version: String,
    /// Organization reported in HELLO response. Default: `"seedlink-rs"`.
    pub organization: String,
    /// Ring buffer capacity (number of records). Default: `10_000`.
    pub ring_capacity: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            software: "SeedLink".to_owned(),
            version: "v3.1".to_owned(),
            organization: "seedlink-rs".to_owned(),
            ring_capacity: 10_000,
        }
    }
}

/// Handle for triggering graceful server shutdown.
///
/// Obtained via [`SeedLinkServer::shutdown_handle()`]. Calling [`shutdown()`](Self::shutdown)
/// stops the accept loop and all active client handlers.
pub struct ShutdownHandle {
    tx: watch::Sender<bool>,
}

impl ShutdownHandle {
    /// Signal the server to shut down gracefully.
    pub fn shutdown(&self) {
        let _ = self.tx.send(true);
    }
}

/// Async SeedLink v3/v4 server.
///
/// Binds to a TCP port, accepts client connections, and distributes
/// miniSEED records from a shared [`DataStore`].
pub struct SeedLinkServer {
    listener: TcpListener,
    config: ServerConfig,
    store: DataStore,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl SeedLinkServer {
    /// Bind to the given address with default configuration.
    pub async fn bind(addr: &str) -> Result<Self> {
        Self::bind_with_config(addr, ServerConfig::default()).await
    }

    /// Bind to the given address with custom configuration.
    pub async fn bind_with_config(addr: &str, config: ServerConfig) -> Result<Self> {
        let listener = TcpListener::bind(addr).await.map_err(ServerError::Bind)?;
        let store = DataStore::new(config.ring_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        info!(addr, "server bound");
        Ok(Self {
            listener,
            config,
            store,
            shutdown_tx,
            shutdown_rx,
        })
    }

    /// Returns the local address this server is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener.local_addr().map_err(ServerError::Io)
    }

    /// Returns a reference to the shared data store.
    pub fn store(&self) -> &DataStore {
        &self.store
    }

    /// Returns a handle that can be used to trigger graceful shutdown.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            tx: self.shutdown_tx.clone(),
        }
    }

    /// Run the accept loop. Spawns a task per client connection.
    ///
    /// Returns when shutdown is signalled or the listener fails.
    pub async fn run(mut self) {
        loop {
            let (stream, addr) = tokio::select! {
                result = self.listener.accept() => {
                    match result {
                        Ok(conn) => conn,
                        Err(e) => {
                            warn!(error = %e, "accept error");
                            continue;
                        }
                    }
                }
                _ = self.shutdown_rx.changed() => {
                    info!("shutdown signal received, stopping accept loop");
                    break;
                }
            };

            info!(%addr, "accepted connection");
            stream.set_nodelay(true).ok();

            let (read_half, write_half) = stream.into_split();
            let store = self.store.clone();
            let handler_config = HandlerConfig {
                software: self.config.software.clone(),
                version: self.config.version.clone(),
                organization: self.config.organization.clone(),
            };
            let shutdown_rx = self.shutdown_rx.clone();

            tokio::spawn(async move {
                let handler =
                    ClientHandler::new(read_half, write_half, store, handler_config, shutdown_rx);
                handler.run().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use seedlink_rs_client::{ClientConfig, ClientState, OwnedFrame, SeedLinkClient};
    use seedlink_rs_protocol::SequenceNumber;
    use seedlink_rs_protocol::frame::v3;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    /// Build a valid 512-byte miniSEED-like payload with station/network in header.
    fn make_payload(station: &str, network: &str) -> Vec<u8> {
        let mut payload = vec![0u8; v3::PAYLOAD_LEN];
        // miniSEED v2 header: station at bytes 8-12, network at bytes 18-19
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

    async fn start_server() -> (DataStore, String) {
        start_server_with_config(ServerConfig::default()).await
    }

    async fn start_server_with_config(config: ServerConfig) -> (DataStore, String) {
        let server = SeedLinkServer::bind_with_config("127.0.0.1:0", config)
            .await
            .unwrap();
        let addr = server.local_addr().unwrap().to_string();
        let store = server.store().clone();
        tokio::spawn(server.run());
        // Small yield to ensure the accept loop is running
        tokio::task::yield_now().await;
        (store, addr)
    }

    async fn start_server_with_shutdown() -> (DataStore, String, ShutdownHandle) {
        start_server_with_shutdown_and_config(ServerConfig::default()).await
    }

    async fn start_server_with_shutdown_and_config(
        config: ServerConfig,
    ) -> (DataStore, String, ShutdownHandle) {
        let server = SeedLinkServer::bind_with_config("127.0.0.1:0", config)
            .await
            .unwrap();
        let addr = server.local_addr().unwrap().to_string();
        let store = server.store().clone();
        let handle = server.shutdown_handle();
        tokio::spawn(server.run());
        tokio::task::yield_now().await;
        (store, addr, handle)
    }

    // ---- Test 1: hello_response ----

    #[tokio::test]
    async fn hello_response() {
        let (_store, addr) = start_server().await;

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();

        assert_eq!(client.server_info().software, "SeedLink");
        assert_eq!(client.server_info().organization, "seedlink-rs");
        // Client should negotiate v4 since server advertises SLPROTO:4.0
        assert_eq!(client.version(), seedlink_rs_protocol::ProtocolVersion::V4);

        client.bye().await.unwrap();
    }

    // ---- Test 2: station_data_end_receives_frames ----

    #[tokio::test]
    async fn station_data_end_receives_frames() {
        let (store, addr) = start_server().await;

        // Push 2 records before client connects
        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);
        store.push("IU", "ANMO", &payload);

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));
    }

    // ---- Test 3: live_push_during_streaming ----

    #[tokio::test]
    async fn live_push_during_streaming() {
        let (store, addr) = start_server().await;

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Push after streaming has started
        let payload = make_payload("ANMO", "IU");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        store.push("IU", "ANMO", &payload);

        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(1));
    }

    // ---- Test 4: data_resume_from_sequence ----

    #[tokio::test]
    async fn data_resume_from_sequence() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        for _ in 0..5 {
            store.push("IU", "ANMO", &payload);
        }

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        // Resume from seq 3 — should receive seq 4 and 5
        client.data_from(SequenceNumber::new(3)).await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(4));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(5));
    }

    // ---- Test 5: multi_station_subscription ----

    #[tokio::test]
    async fn multi_station_subscription() {
        let (store, addr) = start_server().await;

        store.push("IU", "ANMO", &make_payload("ANMO", "IU"));
        store.push("GE", "WLF", &make_payload("WLF", "GE"));

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.station("WLF", "GE").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));
    }

    // ---- Test 6: station_filtering ----

    #[tokio::test]
    async fn station_filtering() {
        let (store, addr) = start_server().await;

        store.push("IU", "ANMO", &make_payload("ANMO", "IU"));
        store.push("GE", "WLF", &make_payload("WLF", "GE"));
        store.push("IU", "ANMO", &make_payload("ANMO", "IU"));

        // Only subscribe to IU.ANMO
        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(3));
        // seq 2 (GE.WLF) was skipped
    }

    // ---- Test 7: bye_disconnects ----

    #[tokio::test]
    async fn bye_disconnects() {
        let (_store, addr) = start_server().await;

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        assert_eq!(client.state(), ClientState::Connected);

        client.bye().await.unwrap();
        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // ---- Test 8: multiple_concurrent_clients ----

    #[tokio::test]
    async fn multiple_concurrent_clients() {
        let (store, addr) = start_server().await;

        let mut client1 = SeedLinkClient::connect(&addr).await.unwrap();
        client1.station("ANMO", "IU").await.unwrap();
        client1.data().await.unwrap();
        client1.end_stream().await.unwrap();

        let mut client2 = SeedLinkClient::connect(&addr).await.unwrap();
        client2.station("ANMO", "IU").await.unwrap();
        client2.data().await.unwrap();
        client2.end_stream().await.unwrap();

        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);

        let f1 = client1.next_frame().await.unwrap().unwrap();
        let f2 = client2.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));
        assert_eq!(f2.sequence(), SequenceNumber::new(1));
    }

    // ---- Test 9: ring_buffer_eviction ----

    #[tokio::test]
    async fn ring_buffer_eviction() {
        let config = ServerConfig {
            ring_capacity: 3,
            ..ServerConfig::default()
        };
        let (store, addr) = start_server_with_config(config).await;

        let payload = make_payload("ANMO", "IU");
        for _ in 0..5 {
            store.push("IU", "ANMO", &payload);
        }

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Only last 3 records should be in the ring
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(3));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(4));

        let f3 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f3.sequence(), SequenceNumber::new(5));
    }

    // ---- Test 10: unknown_command_error ----

    #[tokio::test]
    async fn unknown_command_error() {
        let (_store, addr) = start_server().await;

        let stream = TcpStream::connect(&addr).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        write_half.write_all(b"FOOBAR\r\n").await.unwrap();
        write_half.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("ERROR"), "expected ERROR, got: {line:?}");
        assert!(line.contains("UNSUPPORTED"));
    }

    // ---- Test 11: slproto_v4_negotiate_and_stream ----

    #[tokio::test]
    async fn slproto_v4_negotiate_and_stream() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);
        store.push("IU", "ANMO", &payload);

        // Default client config: prefer_v4 = true
        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        assert_eq!(client.version(), seedlink_rs_protocol::ProtocolVersion::V4);

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));
        // V4 frame should have station_id
        match &f1 {
            OwnedFrame::V4 { station_id, .. } => {
                assert_eq!(station_id, "IU_ANMO");
            }
            _ => panic!("expected V4 frame, got V3"),
        }

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));
        match &f2 {
            OwnedFrame::V4 { station_id, .. } => {
                assert_eq!(station_id, "IU_ANMO");
            }
            _ => panic!("expected V4 frame, got V3"),
        }
    }

    // ---- Test 12: v3_when_client_does_not_prefer_v4 ----

    #[tokio::test]
    async fn v3_when_client_does_not_prefer_v4() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        assert_eq!(client.version(), seedlink_rs_protocol::ProtocolVersion::V3);

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));
        match &f1 {
            OwnedFrame::V3 { .. } => {} // expected
            _ => panic!("expected V3 frame, got V4"),
        }
    }

    // ---- Test 13: fetch_sends_buffered_then_closes ----

    #[tokio::test]
    async fn fetch_sends_buffered_then_closes() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);
        store.push("IU", "ANMO", &payload);

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));

        // Server should close after sending buffered data
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // ---- Test 14: fetch_with_resume_sequence ----

    #[tokio::test]
    async fn fetch_with_resume_sequence() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        for _ in 0..5 {
            store.push("IU", "ANMO", &payload);
        }

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        // FETCH from seq 3 — should only get 4 and 5
        client.fetch_from(SequenceNumber::new(3)).await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(4));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(5));

        // EOF after buffer exhausted
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // ---- Test 15: graceful_shutdown ----

    #[tokio::test]
    async fn graceful_shutdown() {
        let (store, addr, handle) = start_server_with_shutdown().await;

        // Start a streaming client
        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Push one frame so we know connection is alive
        let payload = make_payload("ANMO", "IU");
        store.push("IU", "ANMO", &payload);
        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(1));

        // Trigger shutdown
        handle.shutdown();

        // Client should get EOF (connection closed by server)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let result = client.next_frame().await.unwrap();
        assert!(result.is_none(), "expected EOF after shutdown");
        assert_eq!(client.state(), ClientState::Disconnected);

        // New connections should fail (server no longer accepting)
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            TcpStream::connect(&addr),
        )
        .await;
        // Either timeout or connection refused
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "expected new connection to fail after shutdown"
        );
    }
}
