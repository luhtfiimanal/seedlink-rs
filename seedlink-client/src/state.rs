use std::time::Duration;

use seedlink_rs_protocol::{PayloadFormat, PayloadSubformat, RawFrame, SequenceNumber};

/// Client connection state machine.
///
/// Transitions: `Disconnected` → `Connected` → `Configured` → `Streaming` → `Disconnected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    /// Not connected to any server.
    Disconnected,
    /// TCP connected and HELLO exchanged; ready for STATION/SELECT.
    Connected,
    /// At least one STATION/DATA configured; ready for END or FETCH.
    Configured,
    /// Binary frame streaming active after END or FETCH.
    Streaming,
}

impl ClientState {
    /// Returns the state name as a static string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connected => "Connected",
            Self::Configured => "Configured",
            Self::Streaming => "Streaming",
        }
    }
}

/// Configuration for [`SeedLinkClient`](crate::SeedLinkClient) connections.
pub struct ClientConfig {
    /// Timeout for the initial TCP connection. Default: 10 seconds.
    pub connect_timeout: Duration,
    /// Timeout for individual read operations (lines and frames). Default: 30 seconds.
    pub read_timeout: Duration,
    /// Whether to attempt SeedLink v4 negotiation. Default: `true`.
    pub prefer_v4: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            prefer_v4: true,
        }
    }
}

/// Information about the connected SeedLink server, parsed from HELLO.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    /// Server software name (e.g., `"SeedLink"`).
    pub software: String,
    /// Server version string (e.g., `"v3.1"`).
    pub version: String,
    /// Server organization line.
    pub organization: String,
    /// Advertised capabilities (e.g., `["SLPROTO:4.0", "SLPROTO:3.1"]`).
    pub capabilities: Vec<String>,
}

/// Network + station identifier used as a key for sequence tracking.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StationKey {
    /// FDSN network code (e.g., `"IU"`).
    pub network: String,
    /// Station code (e.g., `"ANMO"`).
    pub station: String,
}

/// An owned SeedLink frame with its payload copied to the heap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedFrame {
    /// SeedLink v3 frame (8-byte header + 512-byte miniSEED).
    V3 {
        /// 6-digit hex sequence number.
        sequence: SequenceNumber,
        /// miniSEED v2 record (512 bytes).
        payload: Vec<u8>,
    },
    /// SeedLink v4 frame with variable-length payload.
    V4 {
        /// Payload format indicator.
        format: PayloadFormat,
        /// Payload sub-format indicator.
        subformat: PayloadSubformat,
        /// 20-digit decimal sequence number.
        sequence: SequenceNumber,
        /// Station identifier (e.g., `"IU_ANMO"`).
        station_id: String,
        /// Payload bytes.
        payload: Vec<u8>,
    },
}

impl OwnedFrame {
    /// Returns the sequence number of this frame.
    pub fn sequence(&self) -> SequenceNumber {
        match self {
            Self::V3 { sequence, .. } | Self::V4 { sequence, .. } => *sequence,
        }
    }

    /// Returns the payload bytes of this frame.
    pub fn payload(&self) -> &[u8] {
        match self {
            Self::V3 { payload, .. } | Self::V4 { payload, .. } => payload,
        }
    }
}

impl<'a> From<RawFrame<'a>> for OwnedFrame {
    fn from(raw: RawFrame<'a>) -> Self {
        match raw {
            RawFrame::V3 { sequence, payload } => Self::V3 {
                sequence,
                payload: payload.to_vec(),
            },
            RawFrame::V4 {
                format,
                subformat,
                sequence,
                station_id,
                payload,
            } => Self::V4 {
                format,
                subformat,
                sequence,
                station_id: station_id.to_owned(),
                payload: payload.to_vec(),
            },
        }
    }
}
