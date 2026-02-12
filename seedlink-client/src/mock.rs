use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use seedlink_rs_protocol::ProtocolVersion;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub struct MockConfig {
    #[allow(dead_code)]
    pub version: ProtocolVersion,
    pub hello_line1: String,
    pub hello_line2: String,
    pub frames: Vec<Vec<u8>>,
    /// Per-connection frame overrides. When set, `connection_frames[i]` is used
    /// for connection `i`; connections beyond the list fall back to `frames`.
    pub connection_frames: Option<Vec<Vec<Vec<u8>>>>,
    pub accept_slproto: bool,
    pub close_after_stream: bool,
    /// How many sequential connections to accept. Default: 1.
    pub max_connections: usize,
}

impl MockConfig {
    pub fn v3_default(frames: Vec<Vec<u8>>) -> Self {
        Self {
            version: ProtocolVersion::V3,
            hello_line1: "SeedLink v3.1 (2020.075)".to_owned(),
            hello_line2: "Mock Server".to_owned(),
            frames,
            connection_frames: None,
            accept_slproto: false,
            close_after_stream: false,
            max_connections: 1,
        }
    }

    pub fn v4_default(frames: Vec<Vec<u8>>) -> Self {
        Self {
            version: ProtocolVersion::V4,
            hello_line1: "SeedLink v4.0 (mock) :: SLPROTO:4.0 SLPROTO:3.1".to_owned(),
            hello_line2: "Mock Server v4".to_owned(),
            frames,
            connection_frames: None,
            accept_slproto: true,
            close_after_stream: false,
            max_connections: 1,
        }
    }
}

/// Captured commands from all connections, grouped per connection index.
#[derive(Clone, Default)]
pub struct CapturedCommands(Arc<Mutex<Vec<Vec<String>>>>);

impl CapturedCommands {
    /// Returns all commands received across all connections.
    /// Outer vec = per connection, inner vec = commands in order.
    pub fn all(&self) -> Vec<Vec<String>> {
        self.0.lock().unwrap().clone()
    }

    /// Returns commands from a specific connection (0-indexed).
    pub fn connection(&self, idx: usize) -> Vec<String> {
        let guard = self.0.lock().unwrap();
        guard.get(idx).cloned().unwrap_or_default()
    }

    fn start_connection(&self) {
        self.0.lock().unwrap().push(Vec::new());
    }

    fn push(&self, cmd: String) {
        let mut guard = self.0.lock().unwrap();
        if let Some(last) = guard.last_mut() {
            last.push(cmd);
        }
    }
}

pub struct MockServer {
    addr: SocketAddr,
    captured: CapturedCommands,
}

impl MockServer {
    pub async fn start(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = CapturedCommands::default();

        let captured_clone = captured.clone();
        tokio::spawn(async move {
            Self::handle_connections(listener, config, captured_clone).await;
        });

        Self { addr, captured }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Returns the captured commands for inspection in tests.
    pub fn captured(&self) -> &CapturedCommands {
        &self.captured
    }

    async fn handle_connections(
        listener: TcpListener,
        config: MockConfig,
        captured: CapturedCommands,
    ) {
        let config = Arc::new(config);

        for conn_idx in 0..config.max_connections {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };

            captured.start_connection();
            let config = Arc::clone(&config);
            Self::handle_one_connection(stream, &config, &captured, conn_idx).await;
        }
    }

    async fn handle_one_connection(
        stream: tokio::net::TcpStream,
        config: &MockConfig,
        captured: &CapturedCommands,
        conn_idx: usize,
    ) {
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();

        let frames = config
            .connection_frames
            .as_ref()
            .and_then(|cf| cf.get(conn_idx))
            .unwrap_or(&config.frames);

        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }

            let trimmed = line.trim().to_uppercase();
            captured.push(trimmed.clone());

            if trimmed == "HELLO" {
                let response = format!("{}\r\n{}\r\n", config.hello_line1, config.hello_line2);
                if write_half.write_all(response.as_bytes()).await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            } else if trimmed.starts_with("SLPROTO") {
                if config.accept_slproto {
                    if write_half.write_all(b"OK\r\n").await.is_err() {
                        break;
                    }
                } else if write_half
                    .write_all(b"ERROR UNSUPPORTED unsupported command\r\n")
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = write_half.flush().await;
            } else if trimmed.starts_with("STATION")
                || trimmed.starts_with("SELECT")
                || trimmed == "DATA"
                || trimmed.starts_with("DATA ")
                || trimmed.starts_with("TIME ")
            {
                // All servers reply OK to STATION/SELECT/DATA (EXTREPLY behavior)
                if write_half.write_all(b"OK\r\n").await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            } else if trimmed == "END" || trimmed == "FETCH" || trimmed.starts_with("FETCH ") {
                // END/FETCH triggers streaming — no text response, just send frames
                for frame in frames {
                    if write_half.write_all(frame).await.is_err() {
                        break;
                    }
                }
                let _ = write_half.flush().await;
                if config.close_after_stream {
                    break;
                }
            } else if trimmed.starts_with("INFO") {
                for frame in frames {
                    if write_half.write_all(frame).await.is_err() {
                        break;
                    }
                }
                if write_half.write_all(b"END\r\n").await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            } else if trimmed == "BYE" {
                let _ = write_half.shutdown().await;
                break;
            }
        }
    }
}
