//! Async SeedLink server for real-time seismic data distribution.
//!
//! Accept client connections and distribute miniSEED records
//! from configured data sources via a ring buffer that can optionally be
//! journaled to disk, so clients resume with `DATA <seq>` across restarts.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> seedlink_rs_server::Result<()> {
//! use seedlink_rs_server::{SeedLinkServer, ServerConfig, SyncPolicy};
//!
//! let config = ServerConfig {
//!     // Journal the ring buffer so DATA <seq> resume survives restarts.
//!     persist_dir: Some("/var/lib/seedlink/journal".into()),
//!     ..ServerConfig::default()
//! };
//! let server = SeedLinkServer::bind_with_config("0.0.0.0:18000", config).await?;
//! let store = server.store().clone();
//!
//! // In-process connections (e.g. a WebSocket bridge) — no TCP hop:
//! let connector = server.local_connector();
//!
//! tokio::spawn(server.run());
//!
//! // Push data from any source
//! let payload = vec![0u8; 512];
//! store.push("IU", "ANMO", &payload)?;
//!
//! // Speak the SeedLink protocol over an in-process duplex stream
//! let _stream = connector.connect();
//! # Ok(())
//! # }
//! ```

pub(crate) mod connections;
pub mod error;
pub(crate) mod handler;
pub(crate) mod info;
pub(crate) mod journal;
pub(crate) mod select;
pub mod store;
pub(crate) mod time;

pub use error::{Result, ServerError};
pub use journal::SyncPolicy;
pub use store::{DataStore, MAX_PAYLOAD_LEN};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use connections::{ConnectionRegistry, Peer};
use handler::{ClientHandler, HandlerConfig};
use tokio::io::DuplexStream;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{info, warn};

/// Format a SystemTime as "YYYY/MM/DD HH:MM:SS" without chrono.
pub(crate) fn format_timestamp(time: SystemTime) -> String {
    let dur = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    // Simple civil time calculation (UTC)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01
    let mut y = 1970i64;
    let mut remaining_days = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }
    let leap = is_leap(y);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    for &md in &month_days {
        if remaining_days < md {
            break;
        }
        remaining_days -= md;
        m += 1;
    }
    let d = remaining_days + 1;
    let month = m + 1;

    format!("{y:04}/{month:02}/{d:02} {hours:02}:{minutes:02}:{seconds:02}")
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

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
    /// Directory for the on-disk ring buffer journal. When set, the ring is
    /// recovered on startup and `DATA <seq>` resume works across restarts.
    /// Default: `None` (in-memory only).
    pub persist_dir: Option<PathBuf>,
    /// fsync policy for the journal (ignored without `persist_dir`).
    /// Default: [`SyncPolicy::EveryRecord`].
    pub sync_policy: SyncPolicy,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            software: "SeedLink".to_owned(),
            version: "v3.1".to_owned(),
            organization: "seedlink-rs".to_owned(),
            ring_capacity: 10_000,
            persist_dir: None,
            sync_policy: SyncPolicy::EveryRecord,
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
    started: String,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    connections: ConnectionRegistry,
}

impl SeedLinkServer {
    /// Bind to the given address with default configuration.
    pub async fn bind(addr: &str) -> Result<Self> {
        Self::bind_with_config(addr, ServerConfig::default()).await
    }

    /// Bind to the given address with custom configuration.
    ///
    /// With `persist_dir` set, journaled records are recovered before the
    /// listener starts accepting; a journal that cannot be opened is an error.
    pub async fn bind_with_config(addr: &str, config: ServerConfig) -> Result<Self> {
        let listener = TcpListener::bind(addr).await.map_err(ServerError::Bind)?;
        let store = match &config.persist_dir {
            Some(dir) => DataStore::with_journal(config.ring_capacity, dir, config.sync_policy)?,
            None => DataStore::new(config.ring_capacity),
        };
        let started = format_timestamp(SystemTime::now());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let connections = ConnectionRegistry::new();
        info!(addr, "server bound");
        Ok(Self {
            listener,
            config,
            store,
            started,
            shutdown_tx,
            shutdown_rx,
            connections,
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

    /// Returns a connector for opening in-process SeedLink connections.
    ///
    /// Each [`LocalConnector::connect`] call yields one end of a duplex
    /// stream whose other end is served by a regular client handler — the
    /// same protocol as TCP, without the localhost socket hop. Useful for
    /// embedding (e.g. bridging WebSocket clients).
    ///
    /// The connector stays valid after [`run()`](Self::run) consumes the
    /// server, and honors graceful shutdown.
    pub fn local_connector(&self) -> LocalConnector {
        LocalConnector {
            store: self.store.clone(),
            config: Arc::new(self.config.clone()),
            started: self.started.clone(),
            shutdown_rx: self.shutdown_rx.clone(),
            connections: self.connections.clone(),
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

            let conn_id = self.connections.register(Peer::Tcp(addr));
            let (read_half, write_half) = stream.into_split();
            let store = self.store.clone();
            let handler_config = HandlerConfig {
                software: self.config.software.clone(),
                version: self.config.version.clone(),
                organization: self.config.organization.clone(),
                started: self.started.clone(),
            };
            let shutdown_rx = self.shutdown_rx.clone();
            let connections = self.connections.clone();

            tokio::spawn(async move {
                let handler = ClientHandler::new(
                    read_half,
                    write_half,
                    store,
                    handler_config,
                    shutdown_rx,
                    conn_id,
                    connections,
                );
                handler.run().await;
            });
        }
    }
}

/// Opens in-process SeedLink connections served by the same handler as TCP
/// clients. Obtained via [`SeedLinkServer::local_connector`]. Clone is cheap.
#[derive(Clone)]
pub struct LocalConnector {
    store: DataStore,
    config: Arc<ServerConfig>,
    started: String,
    shutdown_rx: watch::Receiver<bool>,
    connections: ConnectionRegistry,
}

impl LocalConnector {
    /// Buffer size of the in-process duplex pipe.
    const DUPLEX_BUF: usize = 64 * 1024;

    /// Open a new in-process connection. The returned stream speaks the
    /// SeedLink protocol exactly like a TCP socket to the server; drop it
    /// to disconnect.
    pub fn connect(&self) -> DuplexStream {
        let (client_io, server_io) = tokio::io::duplex(Self::DUPLEX_BUF);
        let (read_half, write_half) = tokio::io::split(server_io);

        let conn_id = self.connections.register(Peer::Local);
        let handler_config = HandlerConfig {
            software: self.config.software.clone(),
            version: self.config.version.clone(),
            organization: self.config.organization.clone(),
            started: self.started.clone(),
        };
        let store = self.store.clone();
        let shutdown_rx = self.shutdown_rx.clone();
        let connections = self.connections.clone();

        tokio::spawn(async move {
            let handler = ClientHandler::new(
                read_half,
                write_half,
                store,
                handler_config,
                shutdown_rx,
                conn_id,
                connections,
            );
            handler.run().await;
        });

        client_io
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
        store.push("IU", "ANMO", &payload).unwrap();
        store.push("IU", "ANMO", &payload).unwrap();

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
        store.push("IU", "ANMO", &payload).unwrap();

        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(1));
    }

    // ---- Test 4: data_resume_from_sequence ----

    #[tokio::test]
    async fn data_resume_from_sequence() {
        let (store, addr) = start_server().await;

        let payload = make_payload("ANMO", "IU");
        for _ in 0..5 {
            store.push("IU", "ANMO", &payload).unwrap();
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

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store.push("GE", "WLF", &make_payload("WLF", "GE")).unwrap();

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

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store.push("GE", "WLF", &make_payload("WLF", "GE")).unwrap();
        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();

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
        store.push("IU", "ANMO", &payload).unwrap();

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
            store.push("IU", "ANMO", &payload).unwrap();
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
        store.push("IU", "ANMO", &payload).unwrap();
        store.push("IU", "ANMO", &payload).unwrap();

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
        store.push("IU", "ANMO", &payload).unwrap();

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
        store.push("IU", "ANMO", &payload).unwrap();
        store.push("IU", "ANMO", &payload).unwrap();

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
            store.push("IU", "ANMO", &payload).unwrap();
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
        store.push("IU", "ANMO", &payload).unwrap();
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

    // ---- Test 16: info_id_returns_xml ----

    #[tokio::test]
    async fn info_id_returns_xml() {
        let (_store, addr) = start_server().await;

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();

        let frames = client
            .info(seedlink_rs_protocol::InfoLevel::Id)
            .await
            .unwrap();
        assert!(!frames.is_empty(), "expected at least one INFO frame");

        // Extract XML from the frame payload (null-padded)
        let payload = frames[0].payload();
        let xml = String::from_utf8_lossy(payload);
        let xml = xml.trim_end_matches('\0');
        assert!(
            xml.contains("software="),
            "XML should contain software attribute: {xml}"
        );
        assert!(
            xml.contains("organization="),
            "XML should contain organization attribute: {xml}"
        );
        assert!(
            xml.contains("seedlink-rs"),
            "XML should mention seedlink-rs: {xml}"
        );
    }

    // ---- Test 17: info_stations_returns_pushed_stations ----

    #[tokio::test]
    async fn info_stations_returns_pushed_stations() {
        let (store, addr) = start_server().await;

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store.push("GE", "WLF", &make_payload("WLF", "GE")).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();

        let frames = client
            .info(seedlink_rs_protocol::InfoLevel::Stations)
            .await
            .unwrap();
        assert!(!frames.is_empty());

        // Combine all frame payloads into one XML string
        let mut xml = String::new();
        for f in &frames {
            let payload = f.payload();
            let s = String::from_utf8_lossy(payload);
            xml.push_str(s.trim_end_matches('\0'));
        }
        assert!(xml.contains("name=\"ANMO\""), "should list ANMO: {xml}");
        assert!(xml.contains("name=\"WLF\""), "should list WLF: {xml}");
        assert!(xml.contains("network=\"IU\""), "should list IU: {xml}");
        assert!(xml.contains("network=\"GE\""), "should list GE: {xml}");
    }

    // ---- Test 18: info_streams_returns_channel_detail ----

    #[tokio::test]
    async fn info_streams_returns_channel_detail() {
        let (store, addr) = start_server().await;

        let mut payload = make_payload("ANMO", "IU");
        // Set channel BHZ at bytes 15..18, location 00 at bytes 13..15, type D at byte 6
        payload[6] = b'D';
        payload[13] = b'0';
        payload[14] = b'0';
        payload[15] = b'B';
        payload[16] = b'H';
        payload[17] = b'Z';
        store.push("IU", "ANMO", &payload).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();

        let frames = client
            .info(seedlink_rs_protocol::InfoLevel::Streams)
            .await
            .unwrap();
        assert!(!frames.is_empty());

        let mut xml = String::new();
        for f in &frames {
            let payload = f.payload();
            let s = String::from_utf8_lossy(payload);
            xml.push_str(s.trim_end_matches('\0'));
        }
        assert!(xml.contains("seedname=\"BHZ\""), "should list BHZ: {xml}");
        assert!(
            xml.contains("location=\"00\""),
            "should list location 00: {xml}"
        );
        assert!(xml.contains("type=\"D\""), "should list type D: {xml}");
    }

    // ---- Test 19: info_unsupported_level_returns_error ----

    #[tokio::test]
    async fn info_unsupported_level_returns_error() {
        let (_store, addr) = start_server().await;

        let stream = TcpStream::connect(&addr).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        write_half.write_all(b"INFO GAPS\r\n").await.unwrap();
        write_half.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with("ERROR"),
            "expected ERROR for unsupported INFO level, got: {line:?}"
        );
    }

    // ---- Test 20: select_filters_by_channel ----

    #[tokio::test]
    async fn select_filters_by_channel() {
        let (store, addr) = start_server().await;

        // Push BHZ record
        let mut payload_bhz = make_payload("ANMO", "IU");
        payload_bhz[15] = b'B';
        payload_bhz[16] = b'H';
        payload_bhz[17] = b'Z';
        store.push("IU", "ANMO", &payload_bhz).unwrap();

        // Push BHN record
        let mut payload_bhn = make_payload("ANMO", "IU");
        payload_bhn[15] = b'B';
        payload_bhn[16] = b'H';
        payload_bhn[17] = b'N';
        store.push("IU", "ANMO", &payload_bhn).unwrap();

        // Push another BHZ record
        store.push("IU", "ANMO", &payload_bhz).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.select("BHZ").await.unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();

        // Should only receive seq 1 and 3 (BHZ), not seq 2 (BHN)
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(3));

        // EOF
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
    }

    // ---- Test 21: select_wildcard_pattern ----

    #[tokio::test]
    async fn select_wildcard_pattern() {
        let (store, addr) = start_server().await;

        let mut payload_bhz = make_payload("ANMO", "IU");
        payload_bhz[15] = b'B';
        payload_bhz[16] = b'H';
        payload_bhz[17] = b'Z';
        store.push("IU", "ANMO", &payload_bhz).unwrap();

        let mut payload_bhn = make_payload("ANMO", "IU");
        payload_bhn[15] = b'B';
        payload_bhn[16] = b'H';
        payload_bhn[17] = b'N';
        store.push("IU", "ANMO", &payload_bhn).unwrap();

        let mut payload_lhz = make_payload("ANMO", "IU");
        payload_lhz[15] = b'L';
        payload_lhz[16] = b'H';
        payload_lhz[17] = b'Z';
        store.push("IU", "ANMO", &payload_lhz).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.select("BH?").await.unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();

        // Should receive BHZ (seq 1) and BHN (seq 2), but not LHZ (seq 3)
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));

        // EOF
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
    }

    // ---- Test 22: no_select_matches_all_channels ----

    #[tokio::test]
    async fn no_select_matches_all_channels() {
        let (store, addr) = start_server().await;

        let mut payload_bhz = make_payload("ANMO", "IU");
        payload_bhz[15] = b'B';
        payload_bhz[16] = b'H';
        payload_bhz[17] = b'Z';
        store.push("IU", "ANMO", &payload_bhz).unwrap();

        let mut payload_bhn = make_payload("ANMO", "IU");
        payload_bhn[15] = b'B';
        payload_bhn[16] = b'H';
        payload_bhn[17] = b'N';
        store.push("IU", "ANMO", &payload_bhn).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        // No SELECT — should match all channels
        client.data().await.unwrap();
        client.fetch().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(2));

        // EOF
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
    }

    // ---- Helper: set_btime ----

    /// Write BTime fields into a miniSEED payload at bytes 20..30 (big-endian).
    fn set_btime(payload: &mut [u8], year: u16, doy: u16, hour: u8, min: u8, sec: u8) {
        let year_be = year.to_be_bytes();
        payload[20] = year_be[0];
        payload[21] = year_be[1];
        let doy_be = doy.to_be_bytes();
        payload[22] = doy_be[0];
        payload[23] = doy_be[1];
        payload[24] = hour;
        payload[25] = min;
        payload[26] = sec;
        payload[27] = 0; // unused
        payload[28] = 0; // ticks hi
        payload[29] = 0; // ticks lo
    }

    // ---- Test 23: time_filtering_excludes_out_of_range ----

    #[tokio::test]
    async fn time_filtering_excludes_out_of_range() {
        let (store, addr) = start_server().await;

        // Record 1: Jan 15, 2024 (DOY 15) — within range
        let mut payload_jan = make_payload("ANMO", "IU");
        set_btime(&mut payload_jan, 2024, 15, 12, 0, 0);
        store.push("IU", "ANMO", &payload_jan).unwrap();

        // Record 2: Feb 15, 2024 (DOY 46) — out of range
        let mut payload_feb = make_payload("ANMO", "IU");
        set_btime(&mut payload_feb, 2024, 46, 12, 0, 0);
        store.push("IU", "ANMO", &payload_feb).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        // TIME window: Jan 1 to Jan 31, 2024
        client
            .time_window("2024,1,1,0,0,0", Some("2024,1,31,23,59,59"))
            .await
            .unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();

        // Should only receive seq 1 (Jan), not seq 2 (Feb)
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));

        // EOF — Feb record filtered out
        let f2 = client.next_frame().await.unwrap();
        assert!(f2.is_none(), "expected EOF, Feb record should be filtered");
    }

    // ---- Test 24: time_filtering_open_ended ----

    #[tokio::test]
    async fn time_filtering_open_ended() {
        let (store, addr) = start_server().await;

        // Record 1: Dec 2023 (DOY 365) — before start
        let mut payload_dec = make_payload("ANMO", "IU");
        set_btime(&mut payload_dec, 2023, 365, 12, 0, 0);
        store.push("IU", "ANMO", &payload_dec).unwrap();

        // Record 2: Jan 15, 2024 (DOY 15) — after start
        let mut payload_jan = make_payload("ANMO", "IU");
        set_btime(&mut payload_jan, 2024, 15, 12, 0, 0);
        store.push("IU", "ANMO", &payload_jan).unwrap();

        // Record 3: Jun 15, 2024 (DOY 167) — after start
        let mut payload_jun = make_payload("ANMO", "IU");
        set_btime(&mut payload_jun, 2024, 167, 12, 0, 0);
        store.push("IU", "ANMO", &payload_jun).unwrap();

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr, config)
            .await
            .unwrap();
        client.station("ANMO", "IU").await.unwrap();
        // Open-ended TIME: from Jan 1, 2024 onwards (no end)
        client.time_window("2024,1,1,0,0,0", None).await.unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();

        // Should get seq 2 (Jan) and seq 3 (Jun), not seq 1 (Dec 2023)
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(2));

        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(3));

        // EOF
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
    }

    // ---- Test 25: info_connections_lists_active_clients ----

    #[tokio::test]
    async fn info_connections_lists_active_clients() {
        let (_store, addr) = start_server().await;

        // Connect two clients
        let stream1 = TcpStream::connect(&addr).await.unwrap();
        let stream2 = TcpStream::connect(&addr).await.unwrap();

        // Small yield to ensure handlers are spawned
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Use a third client to query INFO CONNECTIONS
        let stream3 = TcpStream::connect(&addr).await.unwrap();
        let (read_half, mut write_half) = stream3.into_split();
        let mut reader = BufReader::new(read_half);

        write_half.write_all(b"INFO CONNECTIONS\r\n").await.unwrap();
        write_half.flush().await.unwrap();

        // Read response frames (binary SL frames + END)
        // The response is INFO frames followed by "END\r\n"
        // Read raw bytes until we see END
        let mut all_data = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                tokio::io::AsyncReadExt::read(&mut reader, &mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            if n == 0 {
                break;
            }
            all_data.extend_from_slice(&buf[..n]);
            // Check if we've received the END marker
            if all_data.windows(5).any(|w| w == b"END\r\n") {
                break;
            }
        }

        let data_str = String::from_utf8_lossy(&all_data);
        // Should contain at least 3 connections (client1, client2, client3)
        let connection_count = data_str.matches("<connection ").count();
        assert!(
            connection_count >= 3,
            "expected at least 3 connections, got {connection_count} in: {data_str}"
        );

        // Keep streams alive until end of test
        drop(stream1);
        drop(stream2);
    }

    // ---- Test 26: useragent_accepted ----

    #[tokio::test]
    async fn useragent_accepted() {
        let (_store, addr) = start_server().await;

        let stream = TcpStream::connect(&addr).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        write_half
            .write_all(b"USERAGENT seedlink-rs-test/1.0\r\n")
            .await
            .unwrap();
        write_half.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with("OK"),
            "expected OK for USERAGENT, got: {line:?}"
        );
    }

    // ---- Test 27: batch_mode_multiple_stations ----

    #[tokio::test]
    async fn batch_mode_multiple_stations() {
        let (store, addr) = start_server().await;

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store.push("GE", "WLF", &make_payload("WLF", "GE")).unwrap();

        let stream = TcpStream::connect(&addr).await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Send BATCH, then multi-station setup + END
        write_half.write_all(b"BATCH\r\n").await.unwrap();
        write_half.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with("OK"),
            "expected OK for BATCH, got: {line:?}"
        );

        write_half.write_all(b"STATION ANMO IU\r\n").await.unwrap();
        write_half.flush().await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "expected OK for STATION IU");

        write_half.write_all(b"DATA\r\n").await.unwrap();
        write_half.flush().await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "expected OK for DATA");

        write_half.write_all(b"STATION WLF GE\r\n").await.unwrap();
        write_half.flush().await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "expected OK for STATION GE");

        write_half.write_all(b"DATA\r\n").await.unwrap();
        write_half.flush().await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "expected OK for DATA 2");

        // Send FETCH to get buffered data then close
        write_half.write_all(b"FETCH\r\n").await.unwrap();
        write_half.flush().await.unwrap();

        // Read two v3 frames (520 bytes each)
        let mut frame1 = vec![0u8; v3::FRAME_LEN];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut frame1)
            .await
            .unwrap();

        let mut frame2 = vec![0u8; v3::FRAME_LEN];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut frame2)
            .await
            .unwrap();

        // Both frames should have SL signature
        assert_eq!(&frame1[0..2], b"SL");
        assert_eq!(&frame2[0..2], b"SL");
    }

    // ---- Test 28: connection_unregistered_on_disconnect ----

    #[tokio::test]
    async fn connection_unregistered_on_disconnect() {
        let (_store, addr) = start_server().await;

        // Connect a client
        let stream = TcpStream::connect(&addr).await.unwrap();
        let (_read_half, mut write_half) = stream.into_split();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Count connections via INFO CONNECTIONS from another client
        let count_before = {
            let s = TcpStream::connect(&addr).await.unwrap();
            let (rh, mut wh) = s.into_split();
            let mut r = BufReader::new(rh);
            wh.write_all(b"INFO CONNECTIONS\r\n").await.unwrap();
            wh.flush().await.unwrap();

            let mut all = Vec::new();
            loop {
                let mut buf = [0u8; 4096];
                let n = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    tokio::io::AsyncReadExt::read(&mut r, &mut buf),
                )
                .await
                .unwrap()
                .unwrap();
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n]);
                if all.windows(5).any(|w| w == b"END\r\n") {
                    break;
                }
            }
            let data = String::from_utf8_lossy(&all);
            data.matches("<connection ").count()
        };

        // Disconnect the first client via BYE
        write_half.write_all(b"BYE\r\n").await.unwrap();
        write_half.flush().await.unwrap();
        // Wait for handler to process BYE and unregister
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        drop(_read_half);

        // Count connections again
        let count_after = {
            let s = TcpStream::connect(&addr).await.unwrap();
            let (rh, mut wh) = s.into_split();
            let mut r = BufReader::new(rh);
            wh.write_all(b"INFO CONNECTIONS\r\n").await.unwrap();
            wh.flush().await.unwrap();

            let mut all = Vec::new();
            loop {
                let mut buf = [0u8; 4096];
                let n = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    tokio::io::AsyncReadExt::read(&mut r, &mut buf),
                )
                .await
                .unwrap()
                .unwrap();
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n]);
                if all.windows(5).any(|w| w == b"END\r\n") {
                    break;
                }
            }
            let data = String::from_utf8_lossy(&all);
            data.matches("<connection ").count()
        };

        // After the first client disconnected, there should be fewer connections.
        // count_before includes: first client + query client
        // count_after includes: only the new query client (first client disconnected)
        assert!(
            count_after < count_before,
            "expected fewer connections after BYE: before={count_before}, after={count_after}"
        );
    }

    // ---- Test 29: resume_across_server_restart ----

    #[tokio::test]
    async fn resume_across_server_restart() {
        let tmp = crate::journal::testutil::TempDir::new("server-restart");
        let payload = make_payload("ANMO", "IU");

        // First server lifetime: push 5 records, then shut down.
        {
            let config = ServerConfig {
                persist_dir: Some(tmp.0.clone()),
                ..ServerConfig::default()
            };
            let server = SeedLinkServer::bind_with_config("127.0.0.1:0", config)
                .await
                .unwrap();
            let store = server.store().clone();
            let handle = server.shutdown_handle();
            tokio::spawn(server.run());
            tokio::task::yield_now().await;

            for _ in 0..5 {
                store.push("IU", "ANMO", &payload).unwrap();
            }
            handle.shutdown();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Second server lifetime: same journal dir, fresh port.
        let config = ServerConfig {
            persist_dir: Some(tmp.0.clone()),
            ..ServerConfig::default()
        };
        let server = SeedLinkServer::bind_with_config("127.0.0.1:0", config)
            .await
            .unwrap();
        let addr = server.local_addr().unwrap().to_string();
        let store = server.store().clone();
        tokio::spawn(server.run());
        tokio::task::yield_now().await;

        // A client that saw seq 3 before the restart resumes and must get 4, 5.
        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        client.station("ANMO", "IU").await.unwrap();
        client.data_from(SequenceNumber::new(3)).await.unwrap();
        client.end_stream().await.unwrap();

        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(4));
        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(5));

        // Live pushes continue the pre-restart numbering — no reuse.
        store.push("IU", "ANMO", &payload).unwrap();
        let f3 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f3.sequence(), SequenceNumber::new(6));
    }

    // ---- Test 30: local_connector_streams_without_tcp ----

    #[tokio::test]
    async fn local_connector_streams_without_tcp() {
        let server = SeedLinkServer::bind("127.0.0.1:0").await.unwrap();
        let store = server.store().clone();
        let connector = server.local_connector();
        tokio::spawn(server.run());
        tokio::task::yield_now().await;

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();

        // Speak raw SeedLink over the in-process duplex stream.
        let stream = connector.connect();
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        write_half.write_all(b"HELLO\r\n").await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("SeedLink"), "HELLO reply: {line:?}");
        line.clear();
        reader.read_line(&mut line).await.unwrap(); // organization line

        write_half.write_all(b"STATION ANMO IU\r\n").await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "STATION reply: {line:?}");

        write_half.write_all(b"DATA\r\n").await.unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "DATA reply: {line:?}");

        write_half.write_all(b"END\r\n").await.unwrap();

        let mut frame = vec![0u8; v3::FRAME_LEN];
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut frame)
            .await
            .unwrap();
        assert_eq!(&frame[0..2], b"SL");
        assert_eq!(&frame[2..8], b"000001");

        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut frame)
            .await
            .unwrap();
        assert_eq!(&frame[2..8], b"000002");

        // Live push must reach the in-process client too.
        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        tokio::io::AsyncReadExt::read_exact(&mut reader, &mut frame)
            .await
            .unwrap();
        assert_eq!(&frame[2..8], b"000003");
    }

    // ---- Test 31: v3_skips_non_512_records ----

    #[tokio::test]
    async fn v3_skips_non_512_records() {
        let (store, addr) = start_server().await;

        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();
        store.push("IU", "ANMO", &[0u8; 256]).unwrap(); // not representable in v3
        store
            .push("IU", "ANMO", &make_payload("ANMO", "IU"))
            .unwrap();

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

        // seq 2 is silently skipped on a v3 connection
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(1));
        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(3));
        let f3 = client.next_frame().await.unwrap();
        assert!(f3.is_none(), "expected EOF after FETCH");
    }

    // ---- Test 32: v4_carries_variable_length_records ----

    #[tokio::test]
    async fn v4_carries_variable_length_records() {
        let (store, addr) = start_server().await;

        // A miniSEED v3-style record: "MS" magic + format version 3, 300 bytes.
        let mut ms3 = vec![0u8; 300];
        ms3[0] = b'M';
        ms3[1] = b'S';
        ms3[2] = 3;
        // Station/network fields for the subscription filter don't exist in
        // this fake record, but network/station routing uses push() args.
        store.push("IU", "ANMO", &ms3).unwrap();

        let mut client = SeedLinkClient::connect(&addr).await.unwrap();
        assert_eq!(client.version(), seedlink_rs_protocol::ProtocolVersion::V4);
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(1));
        match &f {
            OwnedFrame::V4 {
                format, payload, ..
            } => {
                assert_eq!(
                    *format,
                    seedlink_rs_protocol::frame::PayloadFormat::MiniSeed3
                );
                assert_eq!(payload.len(), 300);
            }
            _ => panic!("expected V4 frame"),
        }
    }
}
