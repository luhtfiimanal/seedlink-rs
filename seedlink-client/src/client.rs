use std::collections::HashMap;

use futures_core::Stream;
use seedlink_rs_protocol::{Command, InfoLevel, ProtocolVersion, Response, SequenceNumber};
use tracing::{debug, info, trace, warn};

use crate::connection::Connection;
use crate::error::{ClientError, Result};
use crate::negotiate;
use crate::state::{ClientConfig, ClientState, OwnedFrame, ServerInfo, StationKey};

/// Async SeedLink client for connecting to seismic data servers.
///
/// Implements the SeedLink v3/v4 protocol state machine:
/// `Connected` → `Configured` → `Streaming` → `Disconnected`.
///
/// # Example
///
/// ```no_run
/// # async fn example() -> seedlink_rs_client::Result<()> {
/// use seedlink_rs_client::SeedLinkClient;
///
/// let mut client = SeedLinkClient::connect("rtserve.iris.washington.edu:18000").await?;
/// client.station("ANMO", "IU").await?;
/// client.select("BHZ").await?;
/// client.data().await?;
/// client.end_stream().await?;
///
/// while let Some(frame) = client.next_frame().await? {
///     println!("seq={}, len={}", frame.sequence(), frame.payload().len());
/// }
/// # Ok(())
/// # }
/// ```
pub struct SeedLinkClient {
    connection: Connection,
    state: ClientState,
    version: ProtocolVersion,
    server_info: ServerInfo,
    sequences: HashMap<StationKey, SequenceNumber>,
    config: ClientConfig,
}

impl SeedLinkClient {
    /// Connect to a SeedLink server with default configuration.
    ///
    /// Performs TCP connect, sends HELLO, and negotiates v4 if supported.
    /// On success the client is in [`ClientState::Connected`].
    pub async fn connect(addr: &str) -> Result<Self> {
        Self::connect_with_config(addr, ClientConfig::default()).await
    }

    /// Connect to a SeedLink server with custom [`ClientConfig`].
    ///
    /// Performs TCP connect, sends HELLO, and optionally negotiates v4.
    /// On success the client is in [`ClientState::Connected`].
    pub async fn connect_with_config(addr: &str, config: ClientConfig) -> Result<Self> {
        info!(addr, "connecting");
        let mut connection =
            Connection::connect(addr, config.connect_timeout, config.read_timeout).await?;

        // Send HELLO
        connection
            .send_command(&Command::Hello, ProtocolVersion::V3)
            .await?;

        // Read 2-line hello response
        let line1 = connection.read_line().await?;
        let line2 = connection.read_line().await?;
        let hello = Response::parse_hello(&line1, &line2)?;

        let (software, version_str, extra, organization) = match hello {
            Response::Hello {
                software,
                version,
                extra,
                organization,
            } => (software, version, extra, organization),
            _ => {
                return Err(ClientError::UnexpectedResponse(
                    "expected HELLO response".into(),
                ));
            }
        };

        let capabilities = negotiate::parse_capabilities(&extra);
        let mut protocol_version = ProtocolVersion::V3;

        // Attempt v4 negotiation if preferred and supported
        if config.prefer_v4 && negotiate::supports_v4(&capabilities) {
            connection
                .send_command(
                    &Command::SlProto {
                        version: "4.0".into(),
                    },
                    ProtocolVersion::V4,
                )
                .await?;

            let response_line = connection.read_line().await?;
            let response = Response::parse_line(&response_line)?;
            match response {
                Response::Ok => {
                    protocol_version = ProtocolVersion::V4;
                }
                Response::Error { description, .. } => {
                    warn!(%description, "v4 negotiation failed, falling back to v3");
                }
                _ => {
                    return Err(ClientError::UnexpectedResponse(format!(
                        "expected OK or ERROR for SLPROTO, got: {response_line:?}"
                    )));
                }
            }
        }

        let server_info = ServerInfo {
            software,
            version: version_str,
            organization,
            capabilities,
        };

        info!(version = ?protocol_version, "connected");

        Ok(Self {
            connection,
            state: ClientState::Connected,
            version: protocol_version,
            server_info,
            sequences: HashMap::new(),
            config,
        })
    }

    // -- Accessors --

    /// Returns the negotiated protocol version (V3 or V4).
    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    /// Returns information about the connected server (software, version, capabilities).
    pub fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Returns the current client state.
    pub fn state(&self) -> ClientState {
        self.state
    }

    /// Returns the configuration used for this connection.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    // -- Configuration (Connected|Configured → Configured) --

    /// Select a station and network for data subscription.
    ///
    /// Requires state `Connected` or `Configured`. Transitions to `Configured`.
    /// Server must reply OK; returns [`ClientError::ServerError`] on ERROR.
    pub async fn station(&mut self, station: &str, network: &str) -> Result<()> {
        self.require_state_in(
            &[ClientState::Connected, ClientState::Configured],
            "station",
        )?;

        debug!(station, network, "STATION");
        let cmd = Command::Station {
            station: station.to_owned(),
            network: network.to_owned(),
        };
        self.connection.send_command(&cmd, self.version).await?;

        // All modern servers reply OK/ERROR (EXTREPLY behavior)
        self.read_ok_response("STATION").await?;

        self.state = ClientState::Configured;
        Ok(())
    }

    /// Select channels within the current station subscription.
    ///
    /// `pattern` is a SeedLink channel selector (e.g., `"BHZ"`, `"??.BHZ"`).
    /// Requires state `Connected` or `Configured`. Transitions to `Configured`.
    pub async fn select(&mut self, pattern: &str) -> Result<()> {
        self.require_state_in(&[ClientState::Connected, ClientState::Configured], "select")?;

        debug!(pattern, "SELECT");
        let cmd = Command::Select {
            pattern: pattern.to_owned(),
        };
        self.connection.send_command(&cmd, self.version).await?;

        // All modern servers reply OK/ERROR (EXTREPLY behavior)
        self.read_ok_response("SELECT").await?;

        self.state = ClientState::Configured;
        Ok(())
    }

    // -- Arming (Configured → Configured) --

    /// Arm the current station subscription with DATA (stream from beginning).
    ///
    /// This does NOT start streaming — call [`end_stream()`](Self::end_stream) or
    /// [`fetch()`](Self::fetch) after arming all stations.
    /// Requires state `Configured`. State stays `Configured`.
    pub async fn data(&mut self) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "data")?;

        debug!("DATA");
        let cmd = Command::Data {
            sequence: None,
            start: None,
            end: None,
        };
        self.connection.send_command(&cmd, self.version).await?;

        // Server replies OK/ERROR
        self.read_ok_response("DATA").await?;

        // State stays Configured — END triggers streaming
        Ok(())
    }

    /// Arm the current station subscription with DATA, resuming from `sequence`.
    ///
    /// Requires state `Configured`. State stays `Configured`.
    pub async fn data_from(&mut self, sequence: SequenceNumber) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "data_from")?;

        debug!(%sequence, "DATA (resume)");
        let cmd = Command::Data {
            sequence: Some(sequence),
            start: None,
            end: None,
        };
        self.connection.send_command(&cmd, self.version).await?;

        // Server replies OK/ERROR
        self.read_ok_response("DATA").await?;

        // State stays Configured — END triggers streaming
        Ok(())
    }

    /// Arm the current station subscription with a time window (v3 only).
    ///
    /// Sends `TIME start [end]` to request data within a specific time range.
    /// Requires state `Configured`. State stays `Configured`.
    pub async fn time_window(&mut self, start: &str, end: Option<&str>) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "time_window")?;

        debug!(start, ?end, "TIME");
        let cmd = Command::Time {
            start: start.to_owned(),
            end: end.map(|s| s.to_owned()),
        };
        self.connection.send_command(&cmd, self.version).await?;

        self.read_ok_response("TIME").await?;

        // State stays Configured — END triggers streaming
        Ok(())
    }

    // -- Streaming (Configured → Streaming) --

    /// Send END to trigger continuous binary streaming.
    ///
    /// The server begins sending frames immediately with no text response.
    /// Requires state `Configured`. Transitions to `Streaming`.
    pub async fn end_stream(&mut self) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "end_stream")?;

        self.connection
            .send_command(&Command::End, self.version)
            .await?;

        // END has NO text response — binary streaming starts immediately
        self.state = ClientState::Streaming;
        Ok(())
    }

    /// Send FETCH to stream buffered data then close (v3 only).
    ///
    /// Unlike [`end_stream()`](Self::end_stream), FETCH delivers only what the
    /// server has buffered, then the server closes the connection.
    /// Requires state `Configured`. Transitions to `Streaming`.
    pub async fn fetch(&mut self) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "fetch")?;

        let cmd = Command::Fetch { sequence: None };
        self.connection.send_command(&cmd, self.version).await?;

        self.state = ClientState::Streaming;
        Ok(())
    }

    /// Send FETCH resuming from `sequence` (v3 only).
    ///
    /// Requires state `Configured`. Transitions to `Streaming`.
    pub async fn fetch_from(&mut self, sequence: SequenceNumber) -> Result<()> {
        self.require_state_in(&[ClientState::Configured], "fetch_from")?;

        let cmd = Command::Fetch {
            sequence: Some(sequence),
        };
        self.connection.send_command(&cmd, self.version).await?;

        self.state = ClientState::Streaming;
        Ok(())
    }

    // -- Frame reading (Streaming) --

    /// Read the next SeedLink frame from the server.
    ///
    /// Returns `Ok(Some(frame))` on success, `Ok(None)` on clean EOF
    /// (server closed connection), or `Err` on protocol/timeout errors.
    /// On EOF, state transitions to `Disconnected`.
    /// Requires state `Streaming`.
    pub async fn next_frame(&mut self) -> Result<Option<OwnedFrame>> {
        self.require_state_in(&[ClientState::Streaming], "next_frame")?;

        let result = match self.version {
            ProtocolVersion::V3 => self.connection.read_v3_frame().await,
            ProtocolVersion::V4 => self.connection.read_v4_frame().await,
        };

        match result {
            Ok(frame) => {
                trace!(sequence = %frame.sequence(), "frame received");
                self.track_sequence(&frame);
                Ok(Some(frame))
            }
            Err(ClientError::Disconnected) => {
                self.state = ClientState::Disconnected;
                Ok(None)
            }
            Err(ClientError::Io(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.state = ClientState::Disconnected;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    // -- Stream conversion --

    /// Consume this client and return a [`Stream`] of frames.
    ///
    /// The client must be in `Streaming` state. The stream yields
    /// `Ok(OwnedFrame)` per frame and ends with `None` on EOF.
    pub fn into_stream(self) -> impl Stream<Item = Result<OwnedFrame>> {
        crate::stream::frame_stream(self)
    }

    // -- Utility (any state) --

    /// Request server information at the given detail level.
    ///
    /// Returns a vec of INFO response frames (typically XML payloads).
    /// Can be called in any state.
    pub async fn info(&mut self, level: InfoLevel) -> Result<Vec<OwnedFrame>> {
        let cmd = Command::Info { level };
        self.connection.send_command(&cmd, self.version).await?;

        let mut frames = Vec::new();

        // INFO response: SL frames containing XML, terminated by text line or
        // last frame having '*' in header. Mock sends frames then "END\r\n".
        loop {
            let mut peek = [0u8; 2];
            self.connection.read_exact(&mut peek).await?;

            match &peek {
                b"SL" => {
                    let mut buf = [0u8; seedlink_rs_protocol::frame::v3::FRAME_LEN];
                    buf[0..2].copy_from_slice(&peek);
                    self.connection.read_exact(&mut buf[2..]).await?;
                    let raw = seedlink_rs_protocol::frame::v3::parse(&buf)?;
                    frames.push(OwnedFrame::from(raw));
                }
                b"SE" => {
                    let mut header = [0u8; seedlink_rs_protocol::frame::v4::MIN_HEADER_LEN];
                    header[0..2].copy_from_slice(&peek);
                    self.connection.read_exact(&mut header[2..]).await?;
                    let station_id_len = header[16] as usize;
                    let payload_len =
                        u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
                    let remaining = station_id_len + payload_len;
                    let mut full = Vec::with_capacity(
                        seedlink_rs_protocol::frame::v4::MIN_HEADER_LEN + remaining,
                    );
                    full.extend_from_slice(&header);
                    full.resize(
                        seedlink_rs_protocol::frame::v4::MIN_HEADER_LEN + remaining,
                        0,
                    );
                    self.connection
                        .read_exact(&mut full[seedlink_rs_protocol::frame::v4::MIN_HEADER_LEN..])
                        .await?;
                    let (raw, _) = seedlink_rs_protocol::frame::v4::parse(&full)?;
                    frames.push(OwnedFrame::from(raw));
                }
                _ => {
                    // Text line (END, ERROR, etc.) — read rest and stop
                    let prefix = String::from_utf8_lossy(&peek).to_string();
                    let rest = self.connection.read_line().await?;
                    let _full_line = format!("{prefix}{rest}");
                    break;
                }
            }
        }

        Ok(frames)
    }

    /// Send BYE and close the connection.
    ///
    /// Transitions to `Disconnected`. Can be called in any state.
    pub async fn bye(&mut self) -> Result<()> {
        self.connection
            .send_command(&Command::Bye, self.version)
            .await?;
        self.connection.shutdown().await.ok();
        self.state = ClientState::Disconnected;
        Ok(())
    }

    // -- State (no I/O) --

    /// Returns the last received sequence number for a given network/station pair.
    ///
    /// Returns `None` if no frames have been received for that station.
    pub fn last_sequence(&self, network: &str, station: &str) -> Option<SequenceNumber> {
        let key = StationKey {
            network: network.to_owned(),
            station: station.to_owned(),
        };
        self.sequences.get(&key).copied()
    }

    /// Returns a reference to all tracked network/station → sequence mappings.
    pub fn sequences(&self) -> &HashMap<StationKey, SequenceNumber> {
        &self.sequences
    }

    // -- Private helpers --

    fn require_state_in(&self, allowed: &[ClientState], _method: &str) -> Result<()> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            let expected_static: &'static str = match allowed {
                [ClientState::Connected, ClientState::Configured] => "Connected|Configured",
                [ClientState::Configured] => "Configured",
                [ClientState::Streaming] => "Streaming",
                _ => "valid state",
            };
            Err(ClientError::InvalidState {
                expected: expected_static,
                actual: self.state.as_str(),
            })
        }
    }

    async fn read_ok_response(&mut self, command_name: &str) -> Result<()> {
        let line = self.connection.read_line().await?;
        let response = Response::parse_line(&line)?;
        match response {
            Response::Ok => Ok(()),
            Response::Error {
                code, description, ..
            } => {
                let msg = if let Some(c) = code {
                    format!("{command_name}: {} {description}", c.as_str())
                } else {
                    format!("{command_name}: {description}")
                };
                Err(ClientError::ServerError(msg))
            }
            _ => Err(ClientError::UnexpectedResponse(format!(
                "expected OK for {command_name}, got: {line:?}"
            ))),
        }
    }

    fn track_sequence(&mut self, frame: &OwnedFrame) {
        match frame {
            OwnedFrame::V3 {
                sequence, payload, ..
            } => {
                if payload.len() >= 20 {
                    let station = std::str::from_utf8(&payload[8..13])
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    let network = std::str::from_utf8(&payload[18..20])
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    if !station.is_empty() && !network.is_empty() {
                        self.sequences
                            .insert(StationKey { network, station }, *sequence);
                    }
                }
            }
            OwnedFrame::V4 {
                sequence,
                station_id,
                ..
            } => {
                if let Some((network, station)) = station_id.split_once('_') {
                    self.sequences.insert(
                        StationKey {
                            network: network.to_owned(),
                            station: station.to_owned(),
                        },
                        *sequence,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockConfig, MockServer};
    use seedlink_rs_protocol::frame::{PayloadFormat, PayloadSubformat, v3, v4};

    fn make_v3_frame(seq: u64, station: &str, network: &str) -> Vec<u8> {
        let mut payload = [0u8; v3::PAYLOAD_LEN];
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
        v3::write(SequenceNumber::new(seq), &payload).unwrap()
    }

    fn make_v4_frame(seq: u64, station_id: &str) -> Vec<u8> {
        v4::write(
            PayloadFormat::MiniSeed2,
            PayloadSubformat::Data,
            SequenceNumber::new(seq),
            station_id,
            &[0u8; 64],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn hello_v3() {
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        assert_eq!(client.version(), ProtocolVersion::V3);
        assert_eq!(client.server_info().software, "SeedLink");
        assert_eq!(client.server_info().organization, "Mock Server");
        assert_eq!(client.state(), ClientState::Connected);
    }

    #[tokio::test]
    async fn hello_v4_negotiation() {
        let frames = vec![make_v4_frame(1, "IU_ANMO")];
        let server = MockServer::start(MockConfig::v4_default(frames)).await;

        let client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        assert_eq!(client.version(), ProtocolVersion::V4);
        assert_eq!(client.server_info().organization, "Mock Server v4");
        assert_eq!(client.state(), ClientState::Connected);
    }

    #[tokio::test]
    async fn v4_fallback_to_v3() {
        let config = MockConfig {
            version: ProtocolVersion::V3,
            hello_line1: "SeedLink v3.1 :: SLPROTO:4.0".to_owned(),
            hello_line2: "Fake v4 Server".to_owned(),
            frames: vec![make_v3_frame(1, "ANMO", "IU")],
            connection_frames: None,
            accept_slproto: false,
            close_after_stream: false,
            max_connections: 1,
        };
        let server = MockServer::start(config).await;

        let client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        assert_eq!(client.version(), ProtocolVersion::V3);
    }

    // -- v3 flow: STATION → DATA → END → frames --

    #[tokio::test]
    async fn v3_station_data_end_flow() {
        let frames = vec![
            make_v3_frame(1, "ANMO", "IU"),
            make_v3_frame(2, "ANMO", "IU"),
        ];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        // STATION → OK, state → Configured
        client.station("ANMO", "IU").await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        // DATA → OK, state stays Configured
        client.data().await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        // END → no response, state → Streaming
        client.end_stream().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let frame1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));

        let frame2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(2));
    }

    #[tokio::test]
    async fn v3_station_select_data_end_flow() {
        let frames = vec![
            make_v3_frame(1, "ANMO", "IU"),
            make_v3_frame(2, "ANMO", "IU"),
        ];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.select("BHZ").await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.data().await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.end_stream().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let frame1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));
    }

    // -- v4 flow: STATION → SELECT → DATA → END → frames --

    #[tokio::test]
    async fn v4_station_select_data_end_flow() {
        let frames = vec![make_v4_frame(10, "IU_ANMO"), make_v4_frame(11, "IU_ANMO")];
        let server = MockServer::start(MockConfig::v4_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        assert_eq!(client.version(), ProtocolVersion::V4);

        client.station("ANMO", "IU").await.unwrap();
        client.select("BHZ").await.unwrap();
        client.data().await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.end_stream().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let frame1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(10));

        let frame2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(11));
    }

    // -- Multi-station --

    #[tokio::test]
    async fn multi_station_end_stream() {
        let frames = vec![
            make_v3_frame(1, "ANMO", "IU"),
            make_v3_frame(2, "WLF", "GE"),
        ];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        // STATION → DATA → STATION → DATA → END
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();

        client.station("WLF", "GE").await.unwrap();
        client.data().await.unwrap();

        client.end_stream().await.unwrap();

        let frame1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));

        let frame2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(2));
    }

    // -- Resume from sequence --

    #[tokio::test]
    async fn data_from_resume() {
        let frames = vec![make_v3_frame(100, "ANMO", "IU")];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data_from(SequenceNumber::new(99)).await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.end_stream().await.unwrap();

        let frame = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame.sequence(), SequenceNumber::new(100));
    }

    // -- State machine enforcement --

    #[tokio::test]
    async fn state_machine_enforcement() {
        let server =
            MockServer::start(MockConfig::v3_default(vec![make_v3_frame(1, "ANMO", "IU")])).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        // Cannot call data() in Connected state (need Configured)
        let err = client.data().await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));

        // Cannot call end_stream() in Connected state
        let err = client.end_stream().await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));

        // Cannot call next_frame() in Connected state
        let err = client.next_frame().await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));

        // After station → Configured
        client.station("ANMO", "IU").await.unwrap();

        // Cannot call next_frame() in Configured state
        let err = client.next_frame().await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));

        // data() keeps state as Configured
        client.data().await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        // end_stream() → Streaming
        client.end_stream().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        // Cannot call station() in Streaming state
        let err = client.station("WLF", "GE").await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));
    }

    // -- BYE --

    #[tokio::test]
    async fn bye() {
        let server = MockServer::start(MockConfig::v3_default(vec![])).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.bye().await.unwrap();
        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // -- Sequence tracking --

    #[tokio::test]
    async fn v3_sequence_tracking() {
        let frames = vec![
            make_v3_frame(5, "ANMO", "IU"),
            make_v3_frame(10, "ANMO", "IU"),
            make_v3_frame(3, "WLF", "GE"),
        ];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        client.next_frame().await.unwrap();
        client.next_frame().await.unwrap();
        client.next_frame().await.unwrap();

        assert_eq!(
            client.last_sequence("IU", "ANMO"),
            Some(SequenceNumber::new(10))
        );
        assert_eq!(
            client.last_sequence("GE", "WLF"),
            Some(SequenceNumber::new(3))
        );
        assert_eq!(client.last_sequence("XX", "NONE"), None);
        assert_eq!(client.sequences().len(), 2);
    }

    #[tokio::test]
    async fn v4_sequence_tracking() {
        let frames = vec![make_v4_frame(20, "IU_ANMO"), make_v4_frame(21, "IU_ANMO")];
        let server = MockServer::start(MockConfig::v4_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        client.next_frame().await.unwrap();
        client.next_frame().await.unwrap();

        assert_eq!(
            client.last_sequence("IU", "ANMO"),
            Some(SequenceNumber::new(21))
        );
    }

    // -- Config --

    #[tokio::test]
    async fn connect_no_prefer_v4() {
        let server = MockServer::start(MockConfig::v4_default(vec![])).await;

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let client = SeedLinkClient::connect_with_config(&server.addr().to_string(), config)
            .await
            .unwrap();

        assert_eq!(client.version(), ProtocolVersion::V3);
    }

    // -- Server error handling --

    #[tokio::test]
    async fn server_error_on_station() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(read);
            let mut line = String::new();

            loop {
                line.clear();
                let n = tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    break;
                }
                let trimmed = line.trim().to_uppercase();
                if trimmed == "HELLO" {
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut write,
                        b"SeedLink v3.3\r\nTest\r\n",
                    )
                    .await;
                    let _ = tokio::io::AsyncWriteExt::flush(&mut write).await;
                } else if trimmed.starts_with("STATION") {
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut write,
                        b"ERROR ARGUMENTS bad station\r\n",
                    )
                    .await;
                    let _ = tokio::io::AsyncWriteExt::flush(&mut write).await;
                } else if trimmed == "BYE" {
                    break;
                }
            }
        });

        let config = ClientConfig {
            prefer_v4: false,
            ..ClientConfig::default()
        };
        let mut client = SeedLinkClient::connect_with_config(&addr.to_string(), config)
            .await
            .unwrap();

        let err = client.station("BAD", "XX").await.unwrap_err();
        assert!(matches!(err, ClientError::ServerError(_)));
    }

    // -- EOF handling --

    #[tokio::test]
    async fn next_frame_returns_none_on_eof() {
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let config = MockConfig {
            close_after_stream: true,
            ..MockConfig::v3_default(frames)
        };
        let server = MockServer::start(config).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // First frame → Some
        let frame = client.next_frame().await.unwrap();
        assert!(frame.is_some());
        assert_eq!(frame.unwrap().sequence(), SequenceNumber::new(1));

        // Second frame → None (server closed)
        let frame = client.next_frame().await.unwrap();
        assert!(frame.is_none());

        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // -- Fetch --

    #[tokio::test]
    async fn v3_fetch_flow() {
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let config = MockConfig {
            close_after_stream: true,
            ..MockConfig::v3_default(frames)
        };
        let server = MockServer::start(config).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.fetch().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let frame = client.next_frame().await.unwrap();
        assert!(frame.is_some());

        let frame = client.next_frame().await.unwrap();
        assert!(frame.is_none());
        assert_eq!(client.state(), ClientState::Disconnected);
    }

    // -- TIME window --

    #[tokio::test]
    async fn time_window_v3_flow() {
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.time_window("2024,1,0,0,0", None).await.unwrap();
        assert_eq!(client.state(), ClientState::Configured);

        client.end_stream().await.unwrap();
        assert_eq!(client.state(), ClientState::Streaming);

        let frame = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame.sequence(), SequenceNumber::new(1));
    }

    #[tokio::test]
    async fn time_window_with_end() {
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let server = MockServer::start(MockConfig::v3_default(frames)).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client
            .time_window("2024,1,0,0,0", Some("2024,2,0,0,0"))
            .await
            .unwrap();
        assert_eq!(client.state(), ClientState::Configured);
    }

    #[tokio::test]
    async fn time_window_requires_configured() {
        let server = MockServer::start(MockConfig::v3_default(vec![])).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        // Connected, not Configured — should fail
        let err = client.time_window("2024,1,0,0,0", None).await.unwrap_err();
        assert!(matches!(err, ClientError::InvalidState { .. }));
    }
}
