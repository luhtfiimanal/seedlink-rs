use std::time::Duration;

use seedlink_rs_protocol::frame::{v3, v4};
use seedlink_rs_protocol::{Command, ProtocolVersion};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tracing::{debug, trace, warn};

use crate::error::{ClientError, Result};
use crate::state::OwnedFrame;

pub struct Connection {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
    read_timeout: Duration,
}

impl Connection {
    pub async fn connect(
        addr: &str,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self> {
        debug!(addr, "TCP connecting");
        let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| ClientError::Timeout(connect_timeout))?
            .map_err(ClientError::Io)?;

        stream.set_nodelay(true).ok();

        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
            read_timeout,
        })
    }

    pub async fn send_command(&mut self, cmd: &Command, version: ProtocolVersion) -> Result<()> {
        trace!(?cmd, "sending");
        let bytes = cmd.to_bytes(version)?;
        self.send_raw(&bytes).await
    }

    pub async fn send_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data).await.map_err(ClientError::Io)?;
        self.writer.flush().await.map_err(ClientError::Io)?;
        Ok(())
    }

    pub async fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = tokio::time::timeout(self.read_timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| {
                warn!(timeout = ?self.read_timeout, "read timeout");
                ClientError::Timeout(self.read_timeout)
            })?
            .map_err(ClientError::Io)?;
        if n == 0 {
            return Err(ClientError::Disconnected);
        }
        Ok(line)
    }

    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        tokio::time::timeout(self.read_timeout, self.reader.read_exact(buf))
            .await
            .map_err(|_| ClientError::Timeout(self.read_timeout))?
            .map_err(ClientError::Io)?;
        Ok(())
    }

    pub async fn read_v3_frame(&mut self) -> Result<OwnedFrame> {
        let mut buf = [0u8; v3::FRAME_LEN];
        self.read_exact(&mut buf).await?;
        let raw = v3::parse(&buf)?;
        Ok(OwnedFrame::from(raw))
    }

    pub async fn read_v4_frame(&mut self) -> Result<OwnedFrame> {
        // Read minimum header to determine frame size
        let mut header = [0u8; v4::MIN_HEADER_LEN];
        self.read_exact(&mut header).await?;

        let station_id_len = header[16] as usize;
        let payload_len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let remaining = station_id_len + payload_len;

        let mut full = Vec::with_capacity(v4::MIN_HEADER_LEN + remaining);
        full.extend_from_slice(&header);
        full.resize(v4::MIN_HEADER_LEN + remaining, 0);
        self.read_exact(&mut full[v4::MIN_HEADER_LEN..]).await?;

        let (raw, _consumed) = v4::parse(&full)?;
        Ok(OwnedFrame::from(raw))
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.writer.shutdown().await.map_err(ClientError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seedlink_rs_protocol::SequenceNumber;
    use seedlink_rs_protocol::frame::{PayloadFormat, PayloadSubformat};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    async fn setup_pair() -> (Connection, OwnedWriteHalf, OwnedReadHalf) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (client_stream, server_accept) =
            tokio::join!(async { TcpStream::connect(addr).await.unwrap() }, async {
                listener.accept().await.unwrap()
            });

        let (server_read, server_write) = server_accept.0.into_split();
        let (client_read, client_write) = client_stream.into_split();

        let conn = Connection {
            reader: BufReader::new(client_read),
            writer: BufWriter::new(client_write),
            read_timeout: Duration::from_secs(5),
        };

        (conn, server_write, server_read)
    }

    #[tokio::test]
    async fn send_and_read_line() {
        let (mut conn, mut server_write, _server_read) = setup_pair().await;

        server_write.write_all(b"OK\r\n").await.unwrap();
        server_write.flush().await.unwrap();

        let line = conn.read_line().await.unwrap();
        assert_eq!(line, "OK\r\n");
    }

    #[tokio::test]
    async fn send_command() {
        let (mut conn, _server_write, mut server_read) = setup_pair().await;

        conn.send_command(&Command::Hello, ProtocolVersion::V3)
            .await
            .unwrap();

        let mut buf = vec![0u8; 64];
        let n = server_read.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"HELLO\r\n");
    }

    #[tokio::test]
    async fn read_v3_frame() {
        let (mut conn, mut server_write, _server_read) = setup_pair().await;

        let payload = [0xAA_u8; v3::PAYLOAD_LEN];
        let frame = v3::write(SequenceNumber::new(42), &payload).unwrap();
        server_write.write_all(&frame).await.unwrap();
        server_write.flush().await.unwrap();

        let owned = conn.read_v3_frame().await.unwrap();
        assert_eq!(owned.sequence(), SequenceNumber::new(42));
        assert_eq!(owned.payload(), &payload[..]);
    }

    #[tokio::test]
    async fn read_v4_frame() {
        let (mut conn, mut server_write, _server_read) = setup_pair().await;

        let payload = b"test payload data";
        let frame = v4::write(
            PayloadFormat::MiniSeed2,
            PayloadSubformat::Data,
            SequenceNumber::new(99),
            "IU_ANMO",
            payload,
        )
        .unwrap();
        server_write.write_all(&frame).await.unwrap();
        server_write.flush().await.unwrap();

        let owned = conn.read_v4_frame().await.unwrap();
        assert_eq!(owned.sequence(), SequenceNumber::new(99));
        assert_eq!(owned.payload(), payload);
        match &owned {
            OwnedFrame::V4 { station_id, .. } => assert_eq!(station_id, "IU_ANMO"),
            _ => panic!("expected V4 frame"),
        }
    }

    #[tokio::test]
    async fn read_line_disconnected() {
        let (mut conn, server_write, _server_read) = setup_pair().await;
        drop(server_write);
        drop(_server_read);

        let result = conn.read_line().await;
        assert!(matches!(result, Err(ClientError::Disconnected)));
    }

    #[tokio::test]
    async fn connect_timeout() {
        // Use a non-routable address to trigger timeout
        let result = Connection::connect(
            "192.0.2.1:18000",
            Duration::from_millis(50),
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(result, Err(ClientError::Timeout(_))));
    }

    #[tokio::test]
    async fn read_timeout_triggers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (client_stream, _server_accept) =
            tokio::join!(async { TcpStream::connect(addr).await.unwrap() }, async {
                listener.accept().await.unwrap()
            });

        let (client_read, client_write) = client_stream.into_split();

        let mut conn = Connection {
            reader: BufReader::new(client_read),
            writer: BufWriter::new(client_write),
            read_timeout: Duration::from_millis(50),
        };

        // Server sends nothing — read_line should timeout
        let result = conn.read_line().await;
        assert!(matches!(result, Err(ClientError::Timeout(_))));
    }

    #[tokio::test]
    async fn read_exact_partial() {
        let (mut conn, mut server_write, _server_read) = setup_pair().await;

        // Send data in two parts
        let server_task = tokio::spawn(async move {
            server_write.write_all(b"HEL").await.unwrap();
            server_write.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            server_write.write_all(b"LO").await.unwrap();
            server_write.flush().await.unwrap();
        });

        let mut buf = [0u8; 5];
        conn.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"HELLO");

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn read_line_then_v3_frame() {
        let (mut conn, mut server_write, _server_read) = setup_pair().await;

        // Send a line followed by a v3 frame — tests BufReader mode switching
        let payload = [0x55_u8; v3::PAYLOAD_LEN];
        let frame = v3::write(SequenceNumber::new(7), &payload).unwrap();

        server_write.write_all(b"OK\r\n").await.unwrap();
        server_write.write_all(&frame).await.unwrap();
        server_write.flush().await.unwrap();

        let line = conn.read_line().await.unwrap();
        assert_eq!(line.trim(), "OK");

        let owned = conn.read_v3_frame().await.unwrap();
        assert_eq!(owned.sequence(), SequenceNumber::new(7));
    }
}
