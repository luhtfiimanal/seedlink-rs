use seedlink_rs_protocol::frame::{PayloadFormat, PayloadSubformat, v3, v4};
use seedlink_rs_protocol::{Command, InfoLevel, ProtocolVersion, Response, SequenceNumber};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::watch;
use tracing::{debug, info, trace};

use crate::connections::ConnectionRegistry;
use crate::info as info_xml;
use crate::select::SelectPattern;
use crate::store::{DataStore, Record, Subscription};
use crate::time::TimeWindow;

/// Per-client connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Connected,
    Configured,
    Streaming,
}

/// Server config values needed by the handler.
pub(crate) struct HandlerConfig {
    pub software: String,
    pub version: String,
    pub organization: String,
    pub started: String,
}

/// Per-client connection handler — runs as a spawned tokio task.
///
/// Generic over the transport so the same handler serves TCP sockets and
/// in-process duplex streams (see `LocalConnector`).
pub(crate) struct ClientHandler<R, W> {
    reader: BufReader<R>,
    writer: BufWriter<W>,
    store: DataStore,
    config: HandlerConfig,
    state: State,
    protocol_version: ProtocolVersion,
    subscriptions: Vec<Subscription>,
    resume_seq: Option<u64>,
    shutdown_rx: watch::Receiver<bool>,
    conn_id: u64,
    connections: ConnectionRegistry,
}

impl<R, W> ClientHandler<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(
        read_half: R,
        write_half: W,
        store: DataStore,
        config: HandlerConfig,
        shutdown_rx: watch::Receiver<bool>,
        conn_id: u64,
        connections: ConnectionRegistry,
    ) -> Self {
        Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
            store,
            config,
            state: State::Connected,
            protocol_version: ProtocolVersion::V3,
            subscriptions: Vec::new(),
            resume_seq: None,
            shutdown_rx,
            conn_id,
            connections,
        }
    }

    /// Main loop: read commands, handle them, stream when END/FETCH is received.
    pub async fn run(mut self) {
        info!("client connected");
        let mut line = String::new();

        loop {
            line.clear();

            let n = tokio::select! {
                result = self.reader.read_line(&mut line) => {
                    match result {
                        Ok(n) => n,
                        Err(_) => break,
                    }
                }
                _ = self.shutdown_rx.changed() => {
                    debug!("shutdown received during command loop");
                    break;
                }
            };

            if n == 0 {
                break; // client disconnected
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match Command::parse(trimmed) {
                Ok(cmd) => {
                    debug!(command = %cmd_name(&cmd), "received command");
                    if !self.handle_command(cmd).await {
                        break;
                    }
                }
                Err(_) => {
                    let keyword = trimmed.split_whitespace().next().unwrap_or(trimmed);
                    let resp = Response::Error {
                        code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                        description: format!("unknown command: {keyword}"),
                    };
                    if self.send_response(&resp).await.is_err() {
                        break;
                    }
                }
            }
        }

        self.connections.unregister(self.conn_id);
        info!("client disconnected");
    }

    /// Handle a parsed command. Returns `false` if connection should close.
    async fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Hello => {
                let resp = Response::Hello {
                    software: self.config.software.clone(),
                    version: self.config.version.clone(),
                    extra: ":: SLPROTO:4.0 SLPROTO:3.1".to_owned(),
                    organization: self.config.organization.clone(),
                };
                self.send_response(&resp).await.is_ok()
            }
            Command::SlProto { version } => {
                if version == "4.0" {
                    self.protocol_version = ProtocolVersion::V4;
                    self.connections.update(self.conn_id, |info| {
                        info.protocol_version = ProtocolVersion::V4;
                    });
                    debug!("negotiated v4");
                    self.send_response(&Response::Ok).await.is_ok()
                } else {
                    let resp = Response::Error {
                        code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                        description: format!("unsupported protocol version: {version}"),
                    };
                    self.send_response(&resp).await.is_ok()
                }
            }
            Command::Station { station, network } => {
                self.subscriptions.push(Subscription {
                    network,
                    station,
                    select_patterns: Vec::new(),
                    time_window: None,
                });
                self.state = State::Configured;
                self.connections.update(self.conn_id, |info| {
                    info.state = "Configured".to_owned();
                });
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Select { pattern } => {
                if let Some(sub) = self.subscriptions.last_mut() {
                    if let Some(pat) = SelectPattern::parse(&pattern) {
                        sub.select_patterns.push(pat);
                        self.send_response(&Response::Ok).await.is_ok()
                    } else {
                        let resp = Response::Error {
                            code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                            description: format!("invalid SELECT pattern: {pattern}"),
                        };
                        self.send_response(&resp).await.is_ok()
                    }
                } else {
                    let resp = Response::Error {
                        code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                        description: "SELECT requires prior STATION".to_owned(),
                    };
                    self.send_response(&resp).await.is_ok()
                }
            }
            Command::Data { sequence, .. } => {
                self.set_resume_seq(sequence);
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Fetch { sequence } => {
                self.set_resume_seq(sequence);
                // No response for FETCH — binary streaming starts immediately
                self.state = State::Streaming;
                self.connections.update(self.conn_id, |info| {
                    info.state = "Streaming".to_owned();
                });
                self.stream_frames(false).await;
                false // streaming ended, close connection
            }
            Command::Time { start, end } => {
                if let Some(sub) = self.subscriptions.last_mut() {
                    if let Some(tw) = TimeWindow::parse(&start, end.as_deref()) {
                        sub.time_window = Some(tw);
                        self.send_response(&Response::Ok).await.is_ok()
                    } else {
                        let resp = Response::Error {
                            code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                            description: format!("invalid TIME format: {start}"),
                        };
                        self.send_response(&resp).await.is_ok()
                    }
                } else {
                    let resp = Response::Error {
                        code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                        description: "TIME requires prior STATION".to_owned(),
                    };
                    self.send_response(&resp).await.is_ok()
                }
            }
            Command::End => {
                // No response for END — binary streaming starts immediately
                self.state = State::Streaming;
                self.connections.update(self.conn_id, |info| {
                    info.state = "Streaming".to_owned();
                });
                self.stream_frames(true).await;
                false // streaming ended, close connection
            }
            Command::Bye => false,
            Command::Info { level } => self.handle_info(level).await,
            Command::UserAgent { description } => {
                self.connections.update(self.conn_id, |info| {
                    info.user_agent = Some(description.clone());
                });
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Batch => {
                // Our handler already accumulates STATION+SELECT+DATA before END.
                // BATCH mode just suppresses per-command responses, but for simplicity
                // we acknowledge it.
                self.send_response(&Response::Ok).await.is_ok()
            }
            _ => {
                let resp = Response::Error {
                    code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                    description: format!("unsupported command: {}", cmd_name(&cmd)),
                };
                self.send_response(&resp).await.is_ok()
            }
        }
    }

    /// Record a resume cursor, ignoring sentinel values: `ALL_DATA`/`UNSET`
    /// mean "start from everything available", i.e. no cursor.
    fn set_resume_seq(&mut self, sequence: Option<SequenceNumber>) {
        match sequence {
            Some(seq) if !seq.is_special() => self.resume_seq = Some(seq.value()),
            Some(_) => self.resume_seq = None,
            None => {}
        }
    }

    /// Build a frame for the current protocol version.
    ///
    /// Returns `Ok(None)` when the record cannot be carried on this
    /// connection (v3 frames are fixed at 512-byte payloads).
    fn build_frame(
        &self,
        record: &Record,
    ) -> Result<Option<Vec<u8>>, seedlink_rs_protocol::SeedlinkError> {
        match self.protocol_version {
            ProtocolVersion::V3 => {
                if record.payload.len() != v3::PAYLOAD_LEN {
                    debug!(
                        sequence = record.sequence.value(),
                        len = record.payload.len(),
                        "skipping non-512-byte record on v3 connection"
                    );
                    return Ok(None);
                }
                v3::write(record.sequence, &record.payload).map(Some)
            }
            ProtocolVersion::V4 => {
                let station_id = format!("{}_{}", record.network, record.station);
                // miniSEED v3 records start with "MS" + format version 3.
                let format = if record.payload.get(0..2) == Some(b"MS")
                    && record.payload.get(2) == Some(&3)
                {
                    PayloadFormat::MiniSeed3
                } else {
                    PayloadFormat::MiniSeed2
                };
                v4::write(
                    format,
                    PayloadSubformat::Data,
                    record.sequence,
                    &station_id,
                    &record.payload,
                )
                .map(Some)
            }
        }
    }

    /// Stream frames to client.
    ///
    /// If `continuous` is true (END), loops forever waiting for new data.
    /// If `continuous` is false (FETCH), sends current buffer then returns.
    async fn stream_frames(&mut self, continuous: bool) {
        let mut cursor = self.resume_seq.unwrap_or(0);

        loop {
            // Capture notified BEFORE read to avoid race condition
            let notified = self.store.notified();

            let records = self.store.read_since(cursor, &self.subscriptions);
            if !records.is_empty() {
                for r in &records {
                    match self.build_frame(r) {
                        Ok(Some(frame)) => {
                            if self.writer.write_all(&frame).await.is_err() {
                                return;
                            }
                            trace!(sequence = %r.sequence, "frame sent");
                        }
                        Ok(None) => {} // not representable on this connection — skip
                        Err(_) => return,
                    }
                    cursor = r.sequence.value();
                }
                if self.writer.flush().await.is_err() {
                    return;
                }
                continue;
            }

            // No more buffered data
            if !continuous {
                // FETCH mode: done, let connection close
                return;
            }

            // Continuous mode (END): wait for new data or shutdown
            tokio::select! {
                _ = notified => {}
                _ = self.shutdown_rx.changed() => {
                    debug!("shutdown received during streaming");
                    return;
                }
            }
        }
    }

    /// Handle INFO command — build XML, send as frame(s), then END.
    async fn handle_info(&mut self, level: InfoLevel) -> bool {
        let xml = match level {
            InfoLevel::Id => {
                let software = format!("{} {}", self.config.software, self.config.version);
                info_xml::build_info_id_xml(
                    &software,
                    &self.config.organization,
                    &self.config.started,
                )
            }
            InfoLevel::Stations => {
                let stations = self.store.station_info();
                info_xml::build_info_stations_xml(&stations)
            }
            InfoLevel::Streams => {
                let streams = self.store.stream_info();
                info_xml::build_info_streams_xml(&streams)
            }
            InfoLevel::Connections => {
                let conns = self.connections.snapshot();
                info_xml::build_info_connections_xml(&conns)
            }
            _ => {
                let resp = Response::Error {
                    code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                    description: format!("unsupported INFO level: {level}"),
                };
                return self.send_response(&resp).await.is_ok();
            }
        };

        let xml_bytes = xml.as_bytes();

        // Send as frame(s) depending on protocol version
        match self.protocol_version {
            ProtocolVersion::V3 => {
                // Split XML into 512-byte chunks, null-pad last one
                for chunk in xml_bytes.chunks(v3::PAYLOAD_LEN) {
                    let mut padded = vec![0u8; v3::PAYLOAD_LEN];
                    padded[..chunk.len()].copy_from_slice(chunk);
                    let frame = match v3::write(SequenceNumber::new(0), &padded) {
                        Ok(f) => f,
                        Err(_) => return false,
                    };
                    if self.writer.write_all(&frame).await.is_err() {
                        return false;
                    }
                }
            }
            ProtocolVersion::V4 => {
                let frame = match v4::write(
                    PayloadFormat::Xml,
                    PayloadSubformat::Info,
                    SequenceNumber::new(0),
                    "",
                    xml_bytes,
                ) {
                    Ok(f) => f,
                    Err(_) => return false,
                };
                if self.writer.write_all(&frame).await.is_err() {
                    return false;
                }
            }
        }

        // Terminate with END
        if self.writer.write_all(b"END\r\n").await.is_err() {
            return false;
        }
        self.writer.flush().await.is_ok()
    }

    async fn send_response(&mut self, resp: &Response) -> Result<(), std::io::Error> {
        self.writer.write_all(&resp.to_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

fn cmd_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::Hello => "HELLO",
        Command::Station { .. } => "STATION",
        Command::Select { .. } => "SELECT",
        Command::Data { .. } => "DATA",
        Command::End => "END",
        Command::Bye => "BYE",
        Command::Info { .. } => "INFO",
        Command::Batch => "BATCH",
        Command::Fetch { .. } => "FETCH",
        Command::Time { .. } => "TIME",
        Command::Cat => "CAT",
        Command::SlProto { .. } => "SLPROTO",
        Command::Auth { .. } => "AUTH",
        Command::UserAgent { .. } => "USERAGENT",
        Command::EndFetch => "ENDFETCH",
    }
}
