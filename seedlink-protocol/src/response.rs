use crate::error::{Result, SeedlinkError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Unsupported,
    Unexpected,
    Unauthorized,
    Limit,
    Arguments,
    Auth,
    Internal,
}

impl ErrorCode {
    fn parse(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "UNSUPPORTED" => Some(Self::Unsupported),
            "UNEXPECTED" => Some(Self::Unexpected),
            "UNAUTHORIZED" => Some(Self::Unauthorized),
            "LIMIT" => Some(Self::Limit),
            "ARGUMENTS" => Some(Self::Arguments),
            "AUTH" => Some(Self::Auth),
            "INTERNAL" => Some(Self::Internal),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unsupported => "UNSUPPORTED",
            Self::Unexpected => "UNEXPECTED",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Limit => "LIMIT",
            Self::Arguments => "ARGUMENTS",
            Self::Auth => "AUTH",
            Self::Internal => "INTERNAL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ok,
    Error {
        code: Option<ErrorCode>,
        description: String,
    },
    Hello {
        software: String,
        version: String,
        extra: String,
        organization: String,
    },
    End,
}

impl Response {
    /// Parse a single-line response: OK, ERROR, END.
    pub fn parse_line(line: &str) -> Result<Self> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');

        if line.eq_ignore_ascii_case("OK") {
            return Ok(Self::Ok);
        }

        if line.eq_ignore_ascii_case("END") {
            return Ok(Self::End);
        }

        if line.to_uppercase().starts_with("ERROR") {
            return Self::parse_error(line);
        }

        Err(SeedlinkError::InvalidResponse(format!(
            "unrecognized response: {line:?}"
        )))
    }

    /// Parse a two-line HELLO response.
    ///
    /// Line 1: `"SeedLink v3.1 (2020.075) :: SLPROTO:4.0 SLPROTO:3.1"`
    /// Line 2: `"IRIS DMC"`
    pub fn parse_hello(line1: &str, line2: &str) -> Result<Self> {
        let line1 = line1.trim_end_matches('\n').trim_end_matches('\r');
        let line2 = line2.trim_end_matches('\n').trim_end_matches('\r');

        // Split line1 on "::" to get main part and extra (capabilities)
        let (main_part, extra) = if let Some(idx) = line1.find("::") {
            (line1[..idx].trim(), line1[idx + 2..].trim().to_owned())
        } else {
            (line1.trim(), String::new())
        };

        // Parse "SeedLink v3.1 (2020.075)" or similar
        let mut parts = main_part.split_whitespace();
        let software = parts.next().unwrap_or("").to_owned();
        let version = parts.next().unwrap_or("").to_owned();
        // Remaining part of main line (e.g. "(2020.075)")
        let rest: Vec<&str> = parts.collect();
        let extra_main = rest.join(" ");

        // Combine extra_main and capabilities
        let full_extra = if extra_main.is_empty() {
            extra
        } else if extra.is_empty() {
            extra_main
        } else {
            format!("{extra_main} :: {extra}")
        };

        Ok(Self::Hello {
            software,
            version,
            extra: full_extra,
            organization: line2.to_owned(),
        })
    }

    /// Serialize to wire bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Ok => b"OK\r\n".to_vec(),
            Self::Error { code, description } => {
                if let Some(c) = code {
                    format!("ERROR {} {}\r\n", c.as_str(), description).into_bytes()
                } else {
                    // v3 style: just "ERROR\r\n" or with description
                    if description.is_empty() {
                        b"ERROR\r\n".to_vec()
                    } else {
                        format!("ERROR {description}\r\n").into_bytes()
                    }
                }
            }
            Self::Hello {
                software,
                version,
                extra,
                organization,
            } => {
                let line1 = if extra.is_empty() {
                    format!("{software} {version}")
                } else if extra.contains("::") {
                    // Extra already has "::" separator from round-trip
                    format!("{software} {version} {extra}")
                } else {
                    format!("{software} {version} {extra}")
                };
                format!("{line1}\r\n{organization}\r\n").into_bytes()
            }
            Self::End => b"END\r\n".to_vec(),
        }
    }

    fn parse_error(line: &str) -> Result<Self> {
        let after_error = line[5..].trim_start(); // skip "ERROR"

        if after_error.is_empty() {
            return Ok(Self::Error {
                code: None,
                description: String::new(),
            });
        }

        // Try to parse error code (first word after ERROR)
        let mut parts = after_error.splitn(2, ' ');
        let first_word = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").to_owned();

        if let Some(code) = ErrorCode::parse(first_word) {
            Ok(Self::Error {
                code: Some(code),
                description: rest,
            })
        } else {
            // No recognized code — entire thing is description
            Ok(Self::Error {
                code: None,
                description: after_error.to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ok() {
        assert_eq!(Response::parse_line("OK").unwrap(), Response::Ok);
        assert_eq!(Response::parse_line("ok").unwrap(), Response::Ok);
        assert_eq!(Response::parse_line("OK\r\n").unwrap(), Response::Ok);
    }

    #[test]
    fn parse_end() {
        assert_eq!(Response::parse_line("END").unwrap(), Response::End);
        assert_eq!(Response::parse_line("end").unwrap(), Response::End);
    }

    #[test]
    fn parse_error_no_code() {
        assert_eq!(
            Response::parse_line("ERROR").unwrap(),
            Response::Error {
                code: None,
                description: String::new(),
            }
        );
    }

    #[test]
    fn parse_error_with_code() {
        assert_eq!(
            Response::parse_line("ERROR UNSUPPORTED unknown command").unwrap(),
            Response::Error {
                code: Some(ErrorCode::Unsupported),
                description: "unknown command".into(),
            }
        );
    }

    #[test]
    fn parse_error_unknown_code_becomes_description() {
        assert_eq!(
            Response::parse_line("ERROR something went wrong").unwrap(),
            Response::Error {
                code: None,
                description: "something went wrong".into(),
            }
        );
    }

    #[test]
    fn parse_error_all_codes() {
        for code in [
            ErrorCode::Unsupported,
            ErrorCode::Unexpected,
            ErrorCode::Unauthorized,
            ErrorCode::Limit,
            ErrorCode::Arguments,
            ErrorCode::Auth,
            ErrorCode::Internal,
        ] {
            let line = format!("ERROR {} test", code.as_str());
            let resp = Response::parse_line(&line).unwrap();
            assert_eq!(
                resp,
                Response::Error {
                    code: Some(code),
                    description: "test".into(),
                }
            );
        }
    }

    #[test]
    fn parse_hello_with_capabilities() {
        let resp = Response::parse_hello(
            "SeedLink v3.1 (2020.075) :: SLPROTO:4.0 SLPROTO:3.1",
            "IRIS DMC",
        )
        .unwrap();
        assert_eq!(
            resp,
            Response::Hello {
                software: "SeedLink".into(),
                version: "v3.1".into(),
                extra: "(2020.075) :: SLPROTO:4.0 SLPROTO:3.1".into(),
                organization: "IRIS DMC".into(),
            }
        );
    }

    #[test]
    fn parse_hello_without_capabilities() {
        let resp = Response::parse_hello("SeedLink v3.1", "GFZ Potsdam").unwrap();
        assert_eq!(
            resp,
            Response::Hello {
                software: "SeedLink".into(),
                version: "v3.1".into(),
                extra: String::new(),
                organization: "GFZ Potsdam".into(),
            }
        );
    }

    #[test]
    fn parse_unknown_response() {
        assert!(Response::parse_line("FOOBAR").is_err());
    }

    #[test]
    fn to_bytes_ok() {
        assert_eq!(Response::Ok.to_bytes(), b"OK\r\n");
    }

    #[test]
    fn to_bytes_end() {
        assert_eq!(Response::End.to_bytes(), b"END\r\n");
    }

    #[test]
    fn to_bytes_error_no_code() {
        let resp = Response::Error {
            code: None,
            description: String::new(),
        };
        assert_eq!(resp.to_bytes(), b"ERROR\r\n");
    }

    #[test]
    fn to_bytes_error_with_code() {
        let resp = Response::Error {
            code: Some(ErrorCode::Unsupported),
            description: "unknown command".into(),
        };
        assert_eq!(resp.to_bytes(), b"ERROR UNSUPPORTED unknown command\r\n");
    }

    #[test]
    fn to_bytes_hello() {
        let resp = Response::Hello {
            software: "SeedLink".into(),
            version: "v3.1".into(),
            extra: String::new(),
            organization: "IRIS DMC".into(),
        };
        assert_eq!(resp.to_bytes(), b"SeedLink v3.1\r\nIRIS DMC\r\n");
    }

    #[test]
    fn roundtrip_ok() {
        let bytes = Response::Ok.to_bytes();
        let line = std::str::from_utf8(&bytes).unwrap().trim();
        assert_eq!(Response::parse_line(line).unwrap(), Response::Ok);
    }

    #[test]
    fn roundtrip_end() {
        let bytes = Response::End.to_bytes();
        let line = std::str::from_utf8(&bytes).unwrap().trim();
        assert_eq!(Response::parse_line(line).unwrap(), Response::End);
    }

    #[test]
    fn roundtrip_error_with_code() {
        let original = Response::Error {
            code: Some(ErrorCode::Unauthorized),
            description: "access denied".into(),
        };
        let bytes = original.to_bytes();
        let line = std::str::from_utf8(&bytes).unwrap().trim();
        assert_eq!(Response::parse_line(line).unwrap(), original);
    }
}
