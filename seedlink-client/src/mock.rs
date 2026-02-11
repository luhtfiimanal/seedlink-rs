use std::net::SocketAddr;

use seedlink_protocol::ProtocolVersion;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub struct MockConfig {
    #[allow(dead_code)]
    pub version: ProtocolVersion,
    pub hello_line1: String,
    pub hello_line2: String,
    pub frames: Vec<Vec<u8>>,
    pub accept_slproto: bool,
    pub close_after_stream: bool,
}

impl MockConfig {
    pub fn v3_default(frames: Vec<Vec<u8>>) -> Self {
        Self {
            version: ProtocolVersion::V3,
            hello_line1: "SeedLink v3.1 (2020.075)".to_owned(),
            hello_line2: "Mock Server".to_owned(),
            frames,
            accept_slproto: false,
            close_after_stream: false,
        }
    }

    pub fn v4_default(frames: Vec<Vec<u8>>) -> Self {
        Self {
            version: ProtocolVersion::V4,
            hello_line1: "SeedLink v4.0 (mock) :: SLPROTO:4.0 SLPROTO:3.1".to_owned(),
            hello_line2: "Mock Server v4".to_owned(),
            frames,
            accept_slproto: true,
            close_after_stream: false,
        }
    }
}

pub struct MockServer {
    addr: SocketAddr,
}

impl MockServer {
    pub async fn start(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            Self::handle_connections(listener, config).await;
        });

        Self { addr }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    async fn handle_connections(listener: TcpListener, config: MockConfig) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();

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
            {
                // All servers reply OK to STATION/SELECT/DATA (EXTREPLY behavior)
                if write_half.write_all(b"OK\r\n").await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            } else if trimmed == "END" || trimmed == "FETCH" || trimmed.starts_with("FETCH ") {
                // END/FETCH triggers streaming — no text response, just send frames
                for frame in &config.frames {
                    if write_half.write_all(frame).await.is_err() {
                        break;
                    }
                }
                let _ = write_half.flush().await;
                if config.close_after_stream {
                    break;
                }
            } else if trimmed.starts_with("INFO") {
                for frame in &config.frames {
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
