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

    /// Extract the station key (network + station) from the frame.
    ///
    /// For V3, parses station (bytes 8–12) and network (bytes 18–19) from the
    /// miniSEED payload header. For V4, splits `station_id` on `'_'`.
    ///
    /// Returns `None` if the payload is too short or station info is unreadable.
    pub fn station_key(&self) -> Option<StationKey> {
        match self {
            Self::V3 { payload, .. } => {
                if payload.len() >= 20 {
                    let station = std::str::from_utf8(&payload[8..13]).ok()?.trim().to_owned();
                    let network = std::str::from_utf8(&payload[18..20])
                        .ok()?
                        .trim()
                        .to_owned();
                    if !station.is_empty() && !network.is_empty() {
                        Some(StationKey { network, station })
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Self::V4 { station_id, .. } => {
                station_id
                    .split_once('_')
                    .map(|(network, station)| StationKey {
                        network: network.to_owned(),
                        station: station.to_owned(),
                    })
            }
        }
    }

    /// Decode the payload as a miniSEED record.
    ///
    /// Delegates to [`RawFrame::decode()`] on a borrowed view of this frame.
    pub fn decode(&self) -> seedlink_rs_protocol::Result<seedlink_rs_protocol::DataFrame> {
        self.as_raw_frame().decode()
    }

    fn as_raw_frame(&self) -> RawFrame<'_> {
        match self {
            Self::V3 { sequence, payload } => RawFrame::V3 {
                sequence: *sequence,
                payload,
            },
            Self::V4 {
                format,
                subformat,
                sequence,
                station_id,
                payload,
            } => RawFrame::V4 {
                format: *format,
                subformat: *subformat,
                sequence: *sequence,
                station_id,
                payload,
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_zeroed_payload_returns_err() {
        let frame = OwnedFrame::V3 {
            sequence: SequenceNumber::new(1),
            payload: vec![0u8; 512],
        };
        assert!(frame.decode().is_err());
    }

    #[test]
    fn as_raw_frame_roundtrip() {
        let frame = OwnedFrame::V3 {
            sequence: SequenceNumber::new(42),
            payload: vec![0xAA; 512],
        };
        let raw = frame.as_raw_frame();
        assert_eq!(raw.sequence(), SequenceNumber::new(42));
        assert_eq!(raw.payload().len(), 512);
    }
}
