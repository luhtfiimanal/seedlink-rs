use crate::error::{Result, SeedlinkError};
use crate::version::ProtocolVersion;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum InfoLevel {
    /// Server identification (both v3 and v4).
    Id,
    /// Station list (both v3 and v4).
    Stations,
    /// Stream list (both v3 and v4).
    Streams,
    /// Connection list (both v3 and v4).
    Connections,
    /// Gap information (v3 only).
    Gaps,
    /// All information (v3 only).
    All,
    /// Format information (v4 only).
    Formats,
    /// Capability information (v4 only).
    Capabilities,
}

impl InfoLevel {
    /// Parse from string (case-insensitive).
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_uppercase().as_str() {
            "ID" => Ok(Self::Id),
            "STATIONS" => Ok(Self::Stations),
            "STREAMS" => Ok(Self::Streams),
            "CONNECTIONS" => Ok(Self::Connections),
            "GAPS" => Ok(Self::Gaps),
            "ALL" => Ok(Self::All),
            "FORMATS" => Ok(Self::Formats),
            "CAPABILITIES" => Ok(Self::Capabilities),
            _ => Err(SeedlinkError::InvalidInfoLevel(s.to_owned())),
        }
    }

    /// Wire representation (uppercase).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Id => "ID",
            Self::Stations => "STATIONS",
            Self::Streams => "STREAMS",
            Self::Connections => "CONNECTIONS",
            Self::Gaps => "GAPS",
            Self::All => "ALL",
            Self::Formats => "FORMATS",
            Self::Capabilities => "CAPABILITIES",
        }
    }

    /// Check if this info level is valid for the given protocol version.
    pub fn is_valid_for(&self, version: ProtocolVersion) -> bool {
        match self {
            Self::Id | Self::Stations | Self::Streams | Self::Connections => true,
            Self::Gaps | Self::All => version == ProtocolVersion::V3,
            Self::Formats | Self::Capabilities => version == ProtocolVersion::V4,
        }
    }
}

impl std::fmt::Display for InfoLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_levels() {
        assert_eq!(InfoLevel::parse("ID").unwrap(), InfoLevel::Id);
        assert_eq!(InfoLevel::parse("STATIONS").unwrap(), InfoLevel::Stations);
        assert_eq!(InfoLevel::parse("STREAMS").unwrap(), InfoLevel::Streams);
        assert_eq!(
            InfoLevel::parse("CONNECTIONS").unwrap(),
            InfoLevel::Connections
        );
        assert_eq!(InfoLevel::parse("GAPS").unwrap(), InfoLevel::Gaps);
        assert_eq!(InfoLevel::parse("ALL").unwrap(), InfoLevel::All);
        assert_eq!(InfoLevel::parse("FORMATS").unwrap(), InfoLevel::Formats);
        assert_eq!(
            InfoLevel::parse("CAPABILITIES").unwrap(),
            InfoLevel::Capabilities
        );
    }

    #[test]
    fn parse_case_insensitive() {
        assert_eq!(InfoLevel::parse("id").unwrap(), InfoLevel::Id);
        assert_eq!(InfoLevel::parse("Stations").unwrap(), InfoLevel::Stations);
        assert_eq!(InfoLevel::parse("streams").unwrap(), InfoLevel::Streams);
    }

    #[test]
    fn parse_invalid() {
        assert!(InfoLevel::parse("UNKNOWN").is_err());
        assert!(InfoLevel::parse("").is_err());
    }

    #[test]
    fn as_str_roundtrip() {
        let levels = [
            InfoLevel::Id,
            InfoLevel::Stations,
            InfoLevel::Streams,
            InfoLevel::Connections,
            InfoLevel::Gaps,
            InfoLevel::All,
            InfoLevel::Formats,
            InfoLevel::Capabilities,
        ];
        for level in levels {
            assert_eq!(InfoLevel::parse(level.as_str()).unwrap(), level);
        }
    }

    #[test]
    fn version_validity() {
        // Both
        assert!(InfoLevel::Id.is_valid_for(ProtocolVersion::V3));
        assert!(InfoLevel::Id.is_valid_for(ProtocolVersion::V4));
        assert!(InfoLevel::Stations.is_valid_for(ProtocolVersion::V3));
        assert!(InfoLevel::Stations.is_valid_for(ProtocolVersion::V4));

        // v3 only
        assert!(InfoLevel::Gaps.is_valid_for(ProtocolVersion::V3));
        assert!(!InfoLevel::Gaps.is_valid_for(ProtocolVersion::V4));
        assert!(InfoLevel::All.is_valid_for(ProtocolVersion::V3));
        assert!(!InfoLevel::All.is_valid_for(ProtocolVersion::V4));

        // v4 only
        assert!(!InfoLevel::Formats.is_valid_for(ProtocolVersion::V3));
        assert!(InfoLevel::Formats.is_valid_for(ProtocolVersion::V4));
        assert!(!InfoLevel::Capabilities.is_valid_for(ProtocolVersion::V3));
        assert!(InfoLevel::Capabilities.is_valid_for(ProtocolVersion::V4));
    }
}
