use seedlink_rs_protocol::frame::{PayloadFormat, PayloadSubformat, v3, v4};
use seedlink_rs_protocol::{Command, ProtocolVersion, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;
use tracing::{debug, info, trace};

use crate::store::{DataStore, Record, Subscription};

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
}

/// Per-client connection handler — runs as a spawned tokio task.
pub(crate) struct ClientHandler {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
    store: DataStore,
    config: HandlerConfig,
    state: State,
    protocol_version: ProtocolVersion,
    subscriptions: Vec<Subscription>,
    resume_seq: Option<u64>,
    shutdown_rx: watch::Receiver<bool>,
}

impl ClientHandler {
    pub fn new(
        read_half: OwnedReadHalf,
        write_half: OwnedWriteHalf,
        store: DataStore,
        config: HandlerConfig,
        shutdown_rx: watch::Receiver<bool>,
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
                self.subscriptions.push(Subscription { network, station });
                self.state = State::Configured;
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Select { .. } => {
                // Accept but ignore pattern filtering (deferred to Chunk 3)
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Data { sequence, .. } => {
                if let Some(seq) = sequence {
                    self.resume_seq = Some(seq.value());
                }
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::Fetch { sequence } => {
                if let Some(seq) = sequence {
                    self.resume_seq = Some(seq.value());
                }
                // No response for FETCH — binary streaming starts immediately
                self.state = State::Streaming;
                self.stream_frames(false).await;
                false // streaming ended, close connection
            }
            Command::Time { .. } => {
                // Accept but ignore time filtering (deferred to Chunk 3)
                self.send_response(&Response::Ok).await.is_ok()
            }
            Command::End => {
                // No response for END — binary streaming starts immediately
                self.state = State::Streaming;
                self.stream_frames(true).await;
                false // streaming ended, close connection
            }
            Command::Bye => false,
            _ => {
                let resp = Response::Error {
                    code: Some(seedlink_rs_protocol::response::ErrorCode::Unsupported),
                    description: format!("unsupported command: {}", cmd_name(&cmd)),
                };
                self.send_response(&resp).await.is_ok()
            }
        }
    }

    /// Build a frame for the current protocol version.
    fn build_frame(&self, record: &Record) -> Result<Vec<u8>, seedlink_rs_protocol::SeedlinkError> {
        match self.protocol_version {
            ProtocolVersion::V3 => v3::write(record.sequence, &record.payload),
            ProtocolVersion::V4 => {
                let station_id = format!("{}_{}", record.network, record.station);
                v4::write(
                    PayloadFormat::MiniSeed2,
                    PayloadSubformat::Data,
                    record.sequence,
                    &station_id,
                    &record.payload,
                )
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
                    let frame = match self.build_frame(r) {
                        Ok(f) => f,
                        Err(_) => return,
                    };
                    if self.writer.write_all(&frame).await.is_err() {
                        return;
                    }
                    trace!(sequence = %r.sequence, "frame sent");
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
