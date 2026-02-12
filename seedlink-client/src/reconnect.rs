use std::collections::HashMap;
use std::time::Duration;

use futures_core::Stream;
use seedlink_rs_protocol::SequenceNumber;
use tracing::{debug, info, warn};

use crate::SeedLinkClient;
use crate::error::{ClientError, Result};
use crate::state::{ClientConfig, OwnedFrame, StationKey};

/// Configuration for automatic reconnect with exponential backoff.
#[derive(Clone, Debug)]
pub struct ReconnectConfig {
    /// Initial delay before the first reconnect attempt. Default: 1 second.
    pub initial_backoff: Duration,
    /// Maximum delay between reconnect attempts. Default: 60 seconds.
    pub max_backoff: Duration,
    /// Multiplier applied to backoff after each failed attempt. Default: 2.0.
    pub multiplier: f64,
    /// Maximum number of reconnect attempts. 0 = unlimited. Default: 0.
    pub max_attempts: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            multiplier: 2.0,
            max_attempts: 0,
        }
    }
}

/// Records a subscription step for replay on reconnect.
#[derive(Clone, Debug)]
enum SubscriptionStep {
    Station { station: String, network: String },
    Select { pattern: String },
    Data,
    DataFrom(SequenceNumber),
    TimeWindow { start: String, end: Option<String> },
}

/// A wrapper around [`SeedLinkClient`] that automatically reconnects on disconnect.
///
/// Records all subscription steps (STATION, SELECT, DATA, TIME) and replays them
/// on reconnect. On resume, replaces DATA with DATA-from-sequence using the last
/// tracked sequence numbers.
///
/// # Deduplication guarantee
///
/// SeedLink servers may resend the last frame at the requested sequence number
/// when resuming with `DATA seq`. This client automatically deduplicates frames
/// after reconnect: any frame whose sequence number is ≤ the last tracked
/// sequence for its station is silently dropped. Downstream consumers are
/// guaranteed to never see duplicate frames.
pub struct ReconnectingClient {
    addr: String,
    config: ClientConfig,
    reconnect: ReconnectConfig,
    subscriptions: Vec<SubscriptionStep>,
    client: Option<SeedLinkClient>,
    sequences: HashMap<StationKey, SequenceNumber>,
}

impl ReconnectingClient {
    /// Connect to a SeedLink server with reconnect support using default configs.
    pub async fn connect(addr: &str) -> Result<Self> {
        Self::connect_with_config(addr, ClientConfig::default(), ReconnectConfig::default()).await
    }

    /// Connect with custom client and reconnect configuration.
    pub async fn connect_with_config(
        addr: &str,
        config: ClientConfig,
        reconnect: ReconnectConfig,
    ) -> Result<Self> {
        let client = SeedLinkClient::connect_with_config(addr, config.clone()).await?;
        Ok(Self {
            addr: addr.to_owned(),
            config,
            reconnect,
            subscriptions: Vec::new(),
            client: Some(client),
            sequences: HashMap::new(),
        })
    }

    /// Select a station and network. Records the step for reconnect replay.
    pub async fn station(&mut self, station: &str, network: &str) -> Result<()> {
        self.subscriptions.push(SubscriptionStep::Station {
            station: station.to_owned(),
            network: network.to_owned(),
        });
        self.client_mut()?.station(station, network).await
    }

    /// Select channels. Records the step for reconnect replay.
    pub async fn select(&mut self, pattern: &str) -> Result<()> {
        self.subscriptions.push(SubscriptionStep::Select {
            pattern: pattern.to_owned(),
        });
        self.client_mut()?.select(pattern).await
    }

    /// Arm with DATA. Records the step for reconnect replay.
    pub async fn data(&mut self) -> Result<()> {
        self.subscriptions.push(SubscriptionStep::Data);
        self.client_mut()?.data().await
    }

    /// Arm with DATA from a specific sequence. Records the step for reconnect replay.
    pub async fn data_from(&mut self, sequence: SequenceNumber) -> Result<()> {
        self.subscriptions
            .push(SubscriptionStep::DataFrom(sequence));
        self.client_mut()?.data_from(sequence).await
    }

    /// Arm with TIME window. Records the step for reconnect replay.
    pub async fn time_window(&mut self, start: &str, end: Option<&str>) -> Result<()> {
        self.subscriptions.push(SubscriptionStep::TimeWindow {
            start: start.to_owned(),
            end: end.map(|s| s.to_owned()),
        });
        self.client_mut()?.time_window(start, end).await
    }

    /// Send END to start streaming. Does not record (replayed automatically).
    pub async fn end_stream(&mut self) -> Result<()> {
        self.client_mut()?.end_stream().await
    }

    /// Read the next frame, automatically reconnecting on EOF.
    ///
    /// Returns `Ok(Some(frame))` on success, `Ok(None)` when the stream truly ends
    /// (max attempts exhausted or server sends clean EOF after reconnect),
    /// or `Err` on non-recoverable errors.
    ///
    /// Frames with sequence ≤ the last tracked sequence for their station are
    /// silently dropped (deduplication after reconnect).
    pub async fn next_frame(&mut self) -> Result<Option<OwnedFrame>> {
        loop {
            let result = match self.client.as_mut() {
                Some(client) => client.next_frame().await,
                None => return Err(ClientError::Disconnected),
            };

            match result {
                Ok(Some(frame)) => {
                    // Dedup: skip frames we've already seen (server may resend
                    // the last frame after reconnect with DATA seq)
                    if let Some(key) = frame.station_key()
                        && let Some(&tracked) = self.sequences.get(&key)
                        && frame.sequence() <= tracked
                    {
                        debug!(
                            seq = %frame.sequence(),
                            tracked = %tracked,
                            station = ?key,
                            "skipping duplicate frame"
                        );
                        continue;
                    }

                    // Track sequence from the inner client
                    self.sync_sequences();
                    return Ok(Some(frame));
                }
                Ok(None) => {
                    // EOF — attempt reconnect
                    debug!("stream ended, attempting reconnect");
                    match self.attempt_reconnect().await {
                        Ok(()) => {
                            // Reconnected — loop to read from new connection
                            continue;
                        }
                        Err(ClientError::ReconnectFailed { attempts }) => {
                            warn!(attempts, "reconnect failed, giving up");
                            return Err(ClientError::ReconnectFailed { attempts });
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Consume this client and return a [`Stream`] of frames with auto-reconnect.
    ///
    /// Duplicate frames after reconnect are automatically filtered out.
    pub fn into_stream(self) -> impl Stream<Item = Result<OwnedFrame>> {
        async_stream::try_stream! {
            let mut this = self;
            loop {
                match this.next_frame().await {
                    Ok(Some(frame)) => yield frame,
                    Ok(None) => break,
                    Err(ClientError::ReconnectFailed { .. }) => break,
                    Err(e) => Err(e)?,
                }
            }
        }
    }

    /// Returns the last received sequence number for a given network/station pair.
    pub fn last_sequence(&self, network: &str, station: &str) -> Option<SequenceNumber> {
        let key = StationKey {
            network: network.to_owned(),
            station: station.to_owned(),
        };
        self.sequences.get(&key).copied()
    }

    /// Returns a reference to all tracked sequences.
    pub fn sequences(&self) -> &HashMap<StationKey, SequenceNumber> {
        &self.sequences
    }

    // -- Private helpers --

    fn client_mut(&mut self) -> Result<&mut SeedLinkClient> {
        self.client.as_mut().ok_or(ClientError::Disconnected)
    }

    fn sync_sequences(&mut self) {
        if let Some(client) = &self.client {
            for (key, seq) in client.sequences() {
                self.sequences.insert(key.clone(), *seq);
            }
        }
    }

    /// Try to reconnect and replay subscriptions.
    async fn attempt_reconnect(&mut self) -> Result<()> {
        self.client = None;

        let mut backoff = self.reconnect.initial_backoff;
        let max_attempts = self.reconnect.max_attempts;

        for attempt in 1.. {
            if max_attempts > 0 && attempt > max_attempts {
                return Err(ClientError::ReconnectFailed {
                    attempts: max_attempts,
                });
            }

            info!(attempt, backoff_ms = backoff.as_millis(), "reconnecting");
            tokio::time::sleep(backoff).await;

            match SeedLinkClient::connect_with_config(&self.addr, self.config.clone()).await {
                Ok(mut new_client) => {
                    // Replay subscriptions
                    if let Err(e) = self.replay_subscriptions(&mut new_client).await {
                        warn!(attempt, error = %e, "replay failed, retrying");
                        backoff = self.next_backoff(backoff);
                        continue;
                    }

                    // Send END to resume streaming
                    if let Err(e) = new_client.end_stream().await {
                        warn!(attempt, error = %e, "end_stream failed, retrying");
                        backoff = self.next_backoff(backoff);
                        continue;
                    }

                    info!(attempt, "reconnected successfully");
                    self.client = Some(new_client);
                    return Ok(());
                }
                Err(e) => {
                    warn!(attempt, error = %e, "reconnect attempt failed");
                    backoff = self.next_backoff(backoff);
                }
            }
        }

        unreachable!()
    }

    fn next_backoff(&self, current: Duration) -> Duration {
        let next = current.mul_f64(self.reconnect.multiplier);
        next.min(self.reconnect.max_backoff)
    }

    /// Replay all recorded subscription steps on a new client.
    ///
    /// Replaces bare `Data` steps with `DataFrom(last_seq)` when we have
    /// a tracked sequence for the current station context.
    async fn replay_subscriptions(&self, client: &mut SeedLinkClient) -> Result<()> {
        let mut current_station: Option<StationKey> = None;

        for step in &self.subscriptions {
            match step {
                SubscriptionStep::Station { station, network } => {
                    client.station(station, network).await?;
                    current_station = Some(StationKey {
                        network: network.clone(),
                        station: station.clone(),
                    });
                }
                SubscriptionStep::Select { pattern } => {
                    client.select(pattern).await?;
                }
                SubscriptionStep::Data => {
                    // Try to resume from last known sequence
                    if let Some(ref key) = current_station {
                        if let Some(seq) = self.sequences.get(key) {
                            debug!(%seq, station = %key.station, network = %key.network, "resuming from sequence");
                            client.data_from(*seq).await?;
                        } else {
                            client.data().await?;
                        }
                    } else {
                        client.data().await?;
                    }
                }
                SubscriptionStep::DataFrom(seq) => {
                    // If we have a newer sequence, use that instead
                    if let Some(ref key) = current_station
                        && let Some(tracked) = self.sequences.get(key)
                        && *tracked > *seq
                    {
                        client.data_from(*tracked).await?;
                        continue;
                    }
                    client.data_from(*seq).await?;
                }
                SubscriptionStep::TimeWindow { start, end } => {
                    client.time_window(start, end.as_deref()).await?;
                }
            }
        }

        Ok(())
    }
}

// Clone ClientConfig so we can reuse it across reconnects
impl Clone for ClientConfig {
    fn clone(&self) -> Self {
        Self {
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            prefer_v4: self.prefer_v4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockConfig, MockServer};
    use seedlink_rs_protocol::frame::v3;

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

    #[tokio::test]
    async fn reconnect_on_disconnect() {
        // Connection 0: seq=1, Connection 1: seq=2 (new data after reconnect)
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 2,
            connection_frames: Some(vec![
                vec![make_v3_frame(1, "ANMO", "IU")],
                vec![make_v3_frame(2, "ANMO", "IU")],
            ]),
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            max_attempts: 3,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // First frame from first connection
        let frame1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));

        // Connection closes → auto-reconnect → dedup-clean frame from second connection
        let frame2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(2));
    }

    #[tokio::test]
    async fn reconnect_max_attempts() {
        // Server accepts only 1 connection
        let frames = vec![make_v3_frame(1, "ANMO", "IU")];
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 1,
            ..MockConfig::v3_default(frames)
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(20),
            max_attempts: 2,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // First frame OK
        let frame = client.next_frame().await.unwrap().unwrap();
        assert_eq!(frame.sequence(), SequenceNumber::new(1));

        // EOF → reconnect fails (server only accepts 1 connection)
        let err = client.next_frame().await.unwrap_err();
        assert!(matches!(err, ClientError::ReconnectFailed { attempts: 2 }));
    }

    #[tokio::test]
    async fn reconnect_resumes_sequence_verified_on_wire() {
        // Connection 0: seq=10,11. Connection 1: seq=10,11 (dupes) + seq=12 (new).
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 2,
            connection_frames: Some(vec![
                vec![
                    make_v3_frame(10, "ANMO", "IU"),
                    make_v3_frame(11, "ANMO", "IU"),
                ],
                vec![
                    make_v3_frame(10, "ANMO", "IU"),
                    make_v3_frame(11, "ANMO", "IU"),
                    make_v3_frame(12, "ANMO", "IU"),
                ],
            ]),
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            max_attempts: 3,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Read both frames from first connection
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(10));
        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(11));

        assert_eq!(
            client.last_sequence("IU", "ANMO"),
            Some(SequenceNumber::new(11))
        );

        // Auto-reconnect happens here — seq=10 and seq=11 are deduped, seq=12 returned
        let f3 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f3.sequence(), SequenceNumber::new(12));

        // Verify what was sent on the wire for connection #0 (original)
        let conn0 = server.captured().connection(0);
        assert_eq!(conn0[0], "HELLO");
        assert_eq!(conn0[1], "STATION ANMO IU");
        assert_eq!(conn0[2], "DATA");
        assert_eq!(conn0[3], "END");

        // Verify connection #1 (reconnect) used DATA with sequence
        let conn1 = server.captured().connection(1);
        assert_eq!(conn1[0], "HELLO");
        assert_eq!(conn1[1], "STATION ANMO IU");
        // Key assertion: DATA was replayed as DATA 00000B (hex for 11)
        assert_eq!(conn1[2], "DATA 00000B");
        assert_eq!(conn1[3], "END");
    }

    #[tokio::test]
    async fn reconnect_multi_station_resumes_each_sequence() {
        // Two stations: IU/ANMO (last seq=11) and GE/WLF (last seq=5)
        // Connection 1 includes dupes + new frames for each station
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 2,
            connection_frames: Some(vec![
                vec![
                    make_v3_frame(10, "ANMO", "IU"),
                    make_v3_frame(11, "ANMO", "IU"),
                    make_v3_frame(4, "WLF", "GE"),
                    make_v3_frame(5, "WLF", "GE"),
                ],
                vec![
                    make_v3_frame(11, "ANMO", "IU"), // dupe
                    make_v3_frame(12, "ANMO", "IU"), // new
                    make_v3_frame(5, "WLF", "GE"),   // dupe
                    make_v3_frame(6, "WLF", "GE"),   // new
                ],
            ]),
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            max_attempts: 3,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        // Subscribe two stations
        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.station("WLF", "GE").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Read all 4 frames from connection 0
        for _ in 0..4 {
            client.next_frame().await.unwrap().unwrap();
        }

        // Verify tracked sequences
        assert_eq!(
            client.last_sequence("IU", "ANMO"),
            Some(SequenceNumber::new(11))
        );
        assert_eq!(
            client.last_sequence("GE", "WLF"),
            Some(SequenceNumber::new(5))
        );

        // Trigger reconnect — dupes (seq=11/ANMO, seq=5/WLF) are skipped
        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(12)); // first non-dupe

        let f = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f.sequence(), SequenceNumber::new(6)); // second non-dupe (WLF)

        // Verify reconnect commands on the wire
        let conn1 = server.captured().connection(1);
        assert_eq!(conn1[0], "HELLO");
        // Station 1: ANMO/IU with DATA resume from seq 11
        assert_eq!(conn1[1], "STATION ANMO IU");
        assert_eq!(conn1[2], "DATA 00000B"); // hex(11) = 00000B
        // Station 2: WLF/GE with DATA resume from seq 5
        assert_eq!(conn1[3], "STATION WLF GE");
        assert_eq!(conn1[4], "DATA 000005"); // hex(5) = 000005
        assert_eq!(conn1[5], "END");
    }

    #[tokio::test]
    async fn reconnect_into_stream() {
        use std::pin::pin;
        use tokio_stream::StreamExt;

        // Connection 0: seq=1, Connection 1: seq=2 (new data)
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 2,
            connection_frames: Some(vec![
                vec![make_v3_frame(1, "ANMO", "IU")],
                vec![make_v3_frame(2, "ANMO", "IU")],
            ]),
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            max_attempts: 1,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let mut stream = pin!(client.into_stream());

        // Frame from first connection
        let frame1 = stream.next().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));

        // Auto-reconnect, dedup-clean frame from second connection
        let frame2 = stream.next().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(2));

        // After second connection closes, max_attempts=1 exhausted → stream ends
        let end = stream.next().await;
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn reconnect_dedup_skips_all_duplicates() {
        // Connection 0: seq=10,11. Connection 1: seq=10,11 (all dupes).
        // Since conn1 has NO new frames, all are skipped → EOF → reconnect fails.
        let config = MockConfig {
            close_after_stream: true,
            max_connections: 2,
            connection_frames: Some(vec![
                vec![
                    make_v3_frame(10, "ANMO", "IU"),
                    make_v3_frame(11, "ANMO", "IU"),
                ],
                vec![
                    make_v3_frame(10, "ANMO", "IU"),
                    make_v3_frame(11, "ANMO", "IU"),
                ],
            ]),
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let reconnect_config = ReconnectConfig {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(20),
            max_attempts: 1,
            ..Default::default()
        };

        let client_config = ClientConfig {
            prefer_v4: false,
            ..Default::default()
        };

        let mut client = ReconnectingClient::connect_with_config(
            &server.addr().to_string(),
            client_config,
            reconnect_config,
        )
        .await
        .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        // Read both frames from first connection
        let f1 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f1.sequence(), SequenceNumber::new(10));
        let f2 = client.next_frame().await.unwrap().unwrap();
        assert_eq!(f2.sequence(), SequenceNumber::new(11));

        // EOF → reconnect → all frames are dupes → EOF → reconnect fails
        let err = client.next_frame().await.unwrap_err();
        assert!(matches!(err, ClientError::ReconnectFailed { attempts: 1 }));
    }
}
