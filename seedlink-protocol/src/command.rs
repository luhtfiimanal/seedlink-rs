use crate::error::{Result, SeedlinkError};
use crate::info::InfoLevel;
use crate::sequence::SequenceNumber;
use crate::version::ProtocolVersion;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    // Both v3 + v4
    Hello,
    Station {
        station: String,
        network: String,
    },
    Select {
        pattern: String,
    },
    Data {
        sequence: Option<SequenceNumber>,
        start: Option<String>,
        end: Option<String>,
    },
    End,
    Bye,
    Info {
        level: InfoLevel,
    },

    // v3 only
    Batch,
    Fetch {
        sequence: Option<SequenceNumber>,
    },
    Time {
        start: String,
        end: Option<String>,
    },
    Cat,

    // v4 only
    SlProto {
        version: String,
    },
    Auth {
        value: String,
    },
    UserAgent {
        description: String,
    },
    EndFetch,
}

impl Command {
    /// Parse a command from a text line (version-agnostic).
    ///
    /// The line should NOT include the trailing `\r\n`.
    pub fn parse(line: &str) -> Result<Self> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        let mut parts = line.split_whitespace();
        let keyword = parts
            .next()
            .ok_or_else(|| SeedlinkError::InvalidCommand("empty command".into()))?;

        match keyword.to_uppercase().as_str() {
            "HELLO" => {
                reject_extra_args(&mut parts, "HELLO")?;
                Ok(Self::Hello)
            }
            "STATION" => {
                let first = parts.next().ok_or_else(|| {
                    SeedlinkError::InvalidCommand("STATION requires arguments".into())
                })?;
                // v4 uses "NET_STA" combined, v3 uses "STA NET" separate
                if let Some(net) = parts.next() {
                    reject_extra_args(&mut parts, "STATION")?;
                    Ok(Self::Station {
                        station: first.to_owned(),
                        network: net.to_owned(),
                    })
                } else {
                    // v4 combined format: NET_STA
                    if let Some((net, sta)) = first.split_once('_') {
                        Ok(Self::Station {
                            station: sta.to_owned(),
                            network: net.to_owned(),
                        })
                    } else {
                        Err(SeedlinkError::InvalidCommand(format!(
                            "STATION: expected 'STA NET' or 'NET_STA', got {first:?}"
                        )))
                    }
                }
            }
            "SELECT" => {
                let pattern = parts.next().ok_or_else(|| {
                    SeedlinkError::InvalidCommand("SELECT requires a pattern".into())
                })?;
                reject_extra_args(&mut parts, "SELECT")?;
                Ok(Self::Select {
                    pattern: pattern.to_owned(),
                })
            }
            "DATA" => {
                let seq_str = parts.next();
                let start = parts.next().map(|s| s.to_owned());
                let end = parts.next().map(|s| s.to_owned());
                let sequence = seq_str.map(parse_sequence).transpose()?;
                Ok(Self::Data {
                    sequence,
                    start,
                    end,
                })
            }
            "END" => {
                reject_extra_args(&mut parts, "END")?;
                Ok(Self::End)
            }
            "BYE" => {
                reject_extra_args(&mut parts, "BYE")?;
                Ok(Self::Bye)
            }
            "INFO" => {
                let level_str = parts
                    .next()
                    .ok_or_else(|| SeedlinkError::InvalidCommand("INFO requires a level".into()))?;
                reject_extra_args(&mut parts, "INFO")?;
                let level = InfoLevel::parse(level_str)?;
                Ok(Self::Info { level })
            }
            "BATCH" => {
                reject_extra_args(&mut parts, "BATCH")?;
                Ok(Self::Batch)
            }
            "FETCH" => {
                let seq_str = parts.next();
                let sequence = seq_str.map(parse_sequence).transpose()?;
                Ok(Self::Fetch { sequence })
            }
            "TIME" => {
                let start = parts
                    .next()
                    .ok_or_else(|| SeedlinkError::InvalidCommand("TIME requires start".into()))?
                    .to_owned();
                let end = parts.next().map(|s| s.to_owned());
                Ok(Self::Time { start, end })
            }
            "CAT" => {
                reject_extra_args(&mut parts, "CAT")?;
                Ok(Self::Cat)
            }
            "SLPROTO" => {
                let version = parts
                    .next()
                    .ok_or_else(|| {
                        SeedlinkError::InvalidCommand("SLPROTO requires version".into())
                    })?
                    .to_owned();
                reject_extra_args(&mut parts, "SLPROTO")?;
                Ok(Self::SlProto { version })
            }
            "AUTH" => {
                // AUTH value may contain spaces (e.g. "AUTH USERPASS user pass")
                let rest: Vec<&str> = parts.collect();
                if rest.is_empty() {
                    return Err(SeedlinkError::InvalidCommand(
                        "AUTH requires a value".into(),
                    ));
                }
                Ok(Self::Auth {
                    value: rest.join(" "),
                })
            }
            "USERAGENT" => {
                let rest: Vec<&str> = parts.collect();
                if rest.is_empty() {
                    return Err(SeedlinkError::InvalidCommand(
                        "USERAGENT requires a description".into(),
                    ));
                }
                Ok(Self::UserAgent {
                    description: rest.join(" "),
                })
            }
            "ENDFETCH" => {
                reject_extra_args(&mut parts, "ENDFETCH")?;
                Ok(Self::EndFetch)
            }
            _ => Err(SeedlinkError::InvalidCommand(format!(
                "unknown command: {keyword:?}"
            ))),
        }
    }

    /// Serialize to wire bytes for the given protocol version.
    ///
    /// Returns `Err(VersionMismatch)` if the command is not valid for the version.
    pub fn to_bytes(&self, version: ProtocolVersion) -> Result<Vec<u8>> {
        if !self.is_valid_for(version) {
            return Err(SeedlinkError::VersionMismatch {
                command: self.command_name(),
                version,
            });
        }
        let line = self.format_line(version);
        Ok(format!("{line}\r\n").into_bytes())
    }

    /// Check if this command is valid for the given protocol version.
    pub fn is_valid_for(&self, version: ProtocolVersion) -> bool {
        match self {
            Self::Hello
            | Self::Station { .. }
            | Self::Select { .. }
            | Self::Data { .. }
            | Self::End
            | Self::Bye
            | Self::Info { .. } => true,
            Self::Batch | Self::Fetch { .. } | Self::Time { .. } | Self::Cat => {
                version == ProtocolVersion::V3
            }
            Self::SlProto { .. } | Self::Auth { .. } | Self::UserAgent { .. } | Self::EndFetch => {
                version == ProtocolVersion::V4
            }
        }
    }

    fn command_name(&self) -> &'static str {
        match self {
            Self::Hello => "HELLO",
            Self::Station { .. } => "STATION",
            Self::Select { .. } => "SELECT",
            Self::Data { .. } => "DATA",
            Self::End => "END",
            Self::Bye => "BYE",
            Self::Info { .. } => "INFO",
            Self::Batch => "BATCH",
            Self::Fetch { .. } => "FETCH",
            Self::Time { .. } => "TIME",
            Self::Cat => "CAT",
            Self::SlProto { .. } => "SLPROTO",
            Self::Auth { .. } => "AUTH",
            Self::UserAgent { .. } => "USERAGENT",
            Self::EndFetch => "ENDFETCH",
        }
    }

    fn format_line(&self, version: ProtocolVersion) -> String {
        match self {
            Self::Hello => "HELLO".into(),
            Self::Station { station, network } => match version {
                ProtocolVersion::V3 => format!("STATION {station} {network}"),
                ProtocolVersion::V4 => format!("STATION {network}_{station}"),
            },
            Self::Select { pattern } => format!("SELECT {pattern}"),
            Self::Data {
                sequence,
                start,
                end,
            } => {
                let mut s = "DATA".to_owned();
                if let Some(seq) = sequence {
                    s.push(' ');
                    s.push_str(&format_sequence(*seq, version));
                }
                if let Some(start_time) = start {
                    s.push(' ');
                    s.push_str(start_time);
                }
                if let Some(end_time) = end {
                    s.push(' ');
                    s.push_str(end_time);
                }
                s
            }
            Self::End => "END".into(),
            Self::Bye => "BYE".into(),
            Self::Info { level } => format!("INFO {}", level.as_str()),
            Self::Batch => "BATCH".into(),
            Self::Fetch { sequence } => match sequence {
                Some(seq) => format!("FETCH {}", format_sequence(*seq, version)),
                None => "FETCH".into(),
            },
            Self::Time { start, end } => match end {
                Some(e) => format!("TIME {start} {e}"),
                None => format!("TIME {start}"),
            },
            Self::Cat => "CAT".into(),
            Self::SlProto { version: v } => format!("SLPROTO {v}"),
            Self::Auth { value } => format!("AUTH {value}"),
            Self::UserAgent { description } => format!("USERAGENT {description}"),
            Self::EndFetch => "ENDFETCH".into(),
        }
    }
}

/// Parse a sequence number from either hex (v3) or decimal (v4) format.
fn parse_sequence(s: &str) -> Result<SequenceNumber> {
    // Try v3 hex first (exactly 6 hex chars), then fall back to decimal
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        SequenceNumber::from_v3_hex(s)
    } else {
        SequenceNumber::from_v4_decimal(s)
    }
}

/// Format a sequence number for the given protocol version.
fn format_sequence(seq: SequenceNumber, version: ProtocolVersion) -> String {
    match version {
        ProtocolVersion::V3 => seq.to_v3_hex(),
        ProtocolVersion::V4 => seq.to_v4_decimal(),
    }
}

fn reject_extra_args(parts: &mut std::str::SplitWhitespace<'_>, command: &str) -> Result<()> {
    if parts.next().is_some() {
        Err(SeedlinkError::InvalidCommand(format!(
            "{command}: unexpected extra arguments"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hello() {
        assert_eq!(Command::parse("HELLO").unwrap(), Command::Hello);
    }

    #[test]
    fn parse_hello_case_insensitive() {
        assert_eq!(Command::parse("hello").unwrap(), Command::Hello);
    }

    #[test]
    fn parse_station_v3() {
        assert_eq!(
            Command::parse("STATION ANMO IU").unwrap(),
            Command::Station {
                station: "ANMO".into(),
                network: "IU".into(),
            }
        );
    }

    #[test]
    fn parse_station_v4() {
        assert_eq!(
            Command::parse("STATION IU_ANMO").unwrap(),
            Command::Station {
                station: "ANMO".into(),
                network: "IU".into(),
            }
        );
    }

    #[test]
    fn parse_select() {
        assert_eq!(
            Command::parse("SELECT ??.BHZ").unwrap(),
            Command::Select {
                pattern: "??.BHZ".into(),
            }
        );
    }

    #[test]
    fn parse_data_no_args() {
        assert_eq!(
            Command::parse("DATA").unwrap(),
            Command::Data {
                sequence: None,
                start: None,
                end: None,
            }
        );
    }

    #[test]
    fn parse_data_with_hex_seq() {
        let cmd = Command::parse("DATA 00001A").unwrap();
        assert_eq!(
            cmd,
            Command::Data {
                sequence: Some(SequenceNumber::new(26)),
                start: None,
                end: None,
            }
        );
    }

    #[test]
    fn parse_data_with_decimal_seq() {
        let cmd = Command::parse("DATA 26").unwrap();
        assert_eq!(
            cmd,
            Command::Data {
                sequence: Some(SequenceNumber::new(26)),
                start: None,
                end: None,
            }
        );
    }

    #[test]
    fn parse_end() {
        assert_eq!(Command::parse("END").unwrap(), Command::End);
    }

    #[test]
    fn parse_bye() {
        assert_eq!(Command::parse("BYE").unwrap(), Command::Bye);
    }

    #[test]
    fn parse_info() {
        assert_eq!(
            Command::parse("INFO ID").unwrap(),
            Command::Info {
                level: InfoLevel::Id,
            }
        );
    }

    #[test]
    fn parse_batch() {
        assert_eq!(Command::parse("BATCH").unwrap(), Command::Batch);
    }

    #[test]
    fn parse_fetch_no_seq() {
        assert_eq!(
            Command::parse("FETCH").unwrap(),
            Command::Fetch { sequence: None }
        );
    }

    #[test]
    fn parse_fetch_with_seq() {
        let cmd = Command::parse("FETCH 00004F").unwrap();
        assert_eq!(
            cmd,
            Command::Fetch {
                sequence: Some(SequenceNumber::new(0x4F))
            }
        );
    }

    #[test]
    fn parse_time() {
        assert_eq!(
            Command::parse("TIME 2024,1,15,0,0,0").unwrap(),
            Command::Time {
                start: "2024,1,15,0,0,0".into(),
                end: None,
            }
        );
    }

    #[test]
    fn parse_time_with_end() {
        assert_eq!(
            Command::parse("TIME 2024,1,15,0,0,0 2024,1,16,0,0,0").unwrap(),
            Command::Time {
                start: "2024,1,15,0,0,0".into(),
                end: Some("2024,1,16,0,0,0".into()),
            }
        );
    }

    #[test]
    fn parse_cat() {
        assert_eq!(Command::parse("CAT").unwrap(), Command::Cat);
    }

    #[test]
    fn parse_slproto() {
        assert_eq!(
            Command::parse("SLPROTO 4.0").unwrap(),
            Command::SlProto {
                version: "4.0".into(),
            }
        );
    }

    #[test]
    fn parse_auth() {
        assert_eq!(
            Command::parse("AUTH USERPASS user pass").unwrap(),
            Command::Auth {
                value: "USERPASS user pass".into(),
            }
        );
    }

    #[test]
    fn parse_useragent() {
        assert_eq!(
            Command::parse("USERAGENT seedlink-rs/0.1").unwrap(),
            Command::UserAgent {
                description: "seedlink-rs/0.1".into(),
            }
        );
    }

    #[test]
    fn parse_endfetch() {
        assert_eq!(Command::parse("ENDFETCH").unwrap(), Command::EndFetch);
    }

    #[test]
    fn parse_empty_error() {
        assert!(Command::parse("").is_err());
    }

    #[test]
    fn parse_unknown_error() {
        assert!(Command::parse("FOOBAR").is_err());
    }

    #[test]
    fn parse_trailing_crlf() {
        assert_eq!(Command::parse("HELLO\r\n").unwrap(), Command::Hello);
    }

    #[test]
    fn to_bytes_hello() {
        let bytes = Command::Hello.to_bytes(ProtocolVersion::V3).unwrap();
        assert_eq!(bytes, b"HELLO\r\n");
    }

    #[test]
    fn to_bytes_station_v3() {
        let cmd = Command::Station {
            station: "ANMO".into(),
            network: "IU".into(),
        };
        assert_eq!(
            cmd.to_bytes(ProtocolVersion::V3).unwrap(),
            b"STATION ANMO IU\r\n"
        );
    }

    #[test]
    fn to_bytes_station_v4() {
        let cmd = Command::Station {
            station: "ANMO".into(),
            network: "IU".into(),
        };
        assert_eq!(
            cmd.to_bytes(ProtocolVersion::V4).unwrap(),
            b"STATION IU_ANMO\r\n"
        );
    }

    #[test]
    fn to_bytes_data_v3_with_seq() {
        let cmd = Command::Data {
            sequence: Some(SequenceNumber::new(26)),
            start: None,
            end: None,
        };
        assert_eq!(
            cmd.to_bytes(ProtocolVersion::V3).unwrap(),
            b"DATA 00001A\r\n"
        );
    }

    #[test]
    fn to_bytes_data_v4_with_seq() {
        let cmd = Command::Data {
            sequence: Some(SequenceNumber::new(26)),
            start: None,
            end: None,
        };
        assert_eq!(cmd.to_bytes(ProtocolVersion::V4).unwrap(), b"DATA 26\r\n");
    }

    #[test]
    fn version_mismatch_batch_v4() {
        let result = Command::Batch.to_bytes(ProtocolVersion::V4);
        assert!(result.is_err());
    }

    #[test]
    fn version_mismatch_slproto_v3() {
        let cmd = Command::SlProto {
            version: "4.0".into(),
        };
        assert!(cmd.to_bytes(ProtocolVersion::V3).is_err());
    }

    #[test]
    fn is_valid_for_both() {
        assert!(Command::Hello.is_valid_for(ProtocolVersion::V3));
        assert!(Command::Hello.is_valid_for(ProtocolVersion::V4));
    }

    #[test]
    fn is_valid_for_v3_only() {
        assert!(Command::Batch.is_valid_for(ProtocolVersion::V3));
        assert!(!Command::Batch.is_valid_for(ProtocolVersion::V4));
    }

    #[test]
    fn is_valid_for_v4_only() {
        assert!(!Command::EndFetch.is_valid_for(ProtocolVersion::V3));
        assert!(Command::EndFetch.is_valid_for(ProtocolVersion::V4));
    }

    #[test]
    fn roundtrip_v3() {
        let commands = vec![
            Command::Hello,
            Command::Station {
                station: "ANMO".into(),
                network: "IU".into(),
            },
            Command::Select {
                pattern: "??.BHZ".into(),
            },
            Command::Data {
                sequence: Some(SequenceNumber::new(0x1A)),
                start: None,
                end: None,
            },
            Command::End,
            Command::Bye,
            Command::Info {
                level: InfoLevel::Id,
            },
            Command::Batch,
            Command::Cat,
        ];
        for cmd in commands {
            let bytes = cmd.to_bytes(ProtocolVersion::V3).unwrap();
            let line = std::str::from_utf8(&bytes).unwrap();
            let parsed = Command::parse(line).unwrap();
            assert_eq!(parsed, cmd, "roundtrip failed for {cmd:?}");
        }
    }

    #[test]
    fn roundtrip_v4() {
        let commands = vec![
            Command::Hello,
            Command::Station {
                station: "ANMO".into(),
                network: "IU".into(),
            },
            Command::Data {
                sequence: Some(SequenceNumber::new(26)),
                start: None,
                end: None,
            },
            Command::End,
            Command::Bye,
            Command::SlProto {
                version: "4.0".into(),
            },
            Command::EndFetch,
        ];
        for cmd in commands {
            let bytes = cmd.to_bytes(ProtocolVersion::V4).unwrap();
            let line = std::str::from_utf8(&bytes).unwrap();
            let parsed = Command::parse(line).unwrap();
            assert_eq!(parsed, cmd, "roundtrip failed for {cmd:?}");
        }
    }
}
