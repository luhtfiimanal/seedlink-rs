//! Integration tests loading Python-generated test vectors.

use std::path::PathBuf;

use seedlink_rs_protocol::frame::{v3, v4};
use seedlink_rs_protocol::{Command, ProtocolVersion, Response, SequenceNumber};

fn vectors_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("pyscripts/test_vectors");
    if dir.exists() {
        Some(dir)
    } else {
        eprintln!(
            "Test vectors not found at {dir:?}. \
             Run: cd pyscripts && uv run python -m pyscripts.generate_vectors"
        );
        None
    }
}

fn load_json(name: &str) -> Option<serde_json::Value> {
    let dir = vectors_dir()?;
    let path = dir.join(format!("{name}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    Some(serde_json::from_str(&content).unwrap())
}

#[test]
fn test_sequence_vectors() {
    let Some(vectors) = load_json("sequences") else {
        return;
    };

    for v in vectors.as_array().unwrap() {
        let hex = v["v3_hex"].as_str().unwrap();
        let value = v["value"].as_u64().unwrap();
        let decimal = v["v4_decimal"].as_str().unwrap();

        // v3 hex → value
        let seq = SequenceNumber::from_v3_hex(hex).unwrap();
        assert_eq!(seq.value(), value, "v3_hex {hex} → value");

        // value → v3 hex
        assert_eq!(
            SequenceNumber::new(value).to_v3_hex(),
            hex,
            "value {value} → v3_hex"
        );

        // v4 decimal → value
        let seq = SequenceNumber::from_v4_decimal(decimal).unwrap();
        assert_eq!(seq.value(), value, "v4_decimal {decimal} → value");

        // value → v4 decimal
        assert_eq!(
            SequenceNumber::new(value).to_v4_decimal(),
            decimal,
            "value {value} → v4_decimal"
        );
    }
}

#[test]
fn test_command_vectors() {
    let Some(vectors) = load_json("commands") else {
        return;
    };

    for v in vectors.as_array().unwrap() {
        let line = v["line"].as_str().unwrap();

        // Parse
        let cmd = Command::parse(line).unwrap_or_else(|e| panic!("failed to parse {line:?}: {e}"));

        // Verify v3 wire format
        if let Some(v3_wire) = v["v3_wire"].as_str() {
            let bytes = cmd
                .to_bytes(ProtocolVersion::V3)
                .unwrap_or_else(|e| panic!("to_bytes(V3) failed for {line:?}: {e}"));
            assert_eq!(
                std::str::from_utf8(&bytes).unwrap(),
                v3_wire,
                "v3 wire for {line:?}"
            );
        } else {
            assert!(
                cmd.to_bytes(ProtocolVersion::V3).is_err(),
                "{line:?} should fail for V3"
            );
        }

        // Verify v4 wire format
        if let Some(v4_wire) = v["v4_wire"].as_str() {
            let bytes = cmd
                .to_bytes(ProtocolVersion::V4)
                .unwrap_or_else(|e| panic!("to_bytes(V4) failed for {line:?}: {e}"));
            assert_eq!(
                std::str::from_utf8(&bytes).unwrap(),
                v4_wire,
                "v4 wire for {line:?}"
            );
        } else {
            assert!(
                cmd.to_bytes(ProtocolVersion::V4).is_err(),
                "{line:?} should fail for V4"
            );
        }

        // Verify parsed command type
        let expected_cmd = v["command"].as_str().unwrap();
        let actual_cmd = format!("{cmd:?}");
        assert!(
            actual_cmd.starts_with(expected_cmd),
            "command type mismatch: expected {expected_cmd}, got {actual_cmd}"
        );

        // Verify parsed fields
        let fields = v["fields"].as_object().unwrap();
        match &cmd {
            Command::Station { station, network } => {
                assert_eq!(station, fields["station"].as_str().unwrap());
                assert_eq!(network, fields["network"].as_str().unwrap());
            }
            Command::Select { pattern } => {
                assert_eq!(pattern, fields["pattern"].as_str().unwrap());
            }
            Command::Data {
                sequence,
                start,
                end,
            } => {
                match (sequence, &fields["sequence"]) {
                    (Some(seq), serde_json::Value::Number(n)) => {
                        assert_eq!(seq.value(), n.as_u64().unwrap());
                    }
                    (None, serde_json::Value::Null) => {}
                    _ => panic!("sequence mismatch for {line:?}"),
                }
                match (start, &fields["start"]) {
                    (Some(s), serde_json::Value::String(expected)) => assert_eq!(s, expected),
                    (None, serde_json::Value::Null) => {}
                    _ => panic!("start mismatch for {line:?}"),
                }
                match (end, &fields["end"]) {
                    (Some(e), serde_json::Value::String(expected)) => assert_eq!(e, expected),
                    (None, serde_json::Value::Null) => {}
                    _ => panic!("end mismatch for {line:?}"),
                }
            }
            Command::Info { level } => {
                assert_eq!(level.as_str(), fields["level"].as_str().unwrap());
            }
            Command::Time { start, end } => {
                assert_eq!(start, fields["start"].as_str().unwrap());
                match (end, &fields["end"]) {
                    (Some(e), serde_json::Value::String(expected)) => assert_eq!(e, expected),
                    (None, serde_json::Value::Null) => {}
                    _ => panic!("end mismatch for {line:?}"),
                }
            }
            Command::SlProto { version } => {
                assert_eq!(version, fields["version"].as_str().unwrap());
            }
            // Fieldless commands
            Command::Hello
            | Command::End
            | Command::Bye
            | Command::Batch
            | Command::Cat
            | Command::EndFetch => {
                assert!(fields.is_empty(), "expected no fields for {expected_cmd}");
            }
            _ => {}
        }
    }
}

#[test]
fn test_response_vectors() {
    let Some(vectors) = load_json("responses") else {
        return;
    };

    for v in vectors.as_array().unwrap() {
        let line = v["line"].as_str().unwrap();
        let resp_type = v["type"].as_str().unwrap();

        let resp =
            Response::parse_line(line).unwrap_or_else(|e| panic!("failed to parse {line:?}: {e}"));

        match resp_type {
            "Ok" => assert_eq!(resp, Response::Ok),
            "End" => assert_eq!(resp, Response::End),
            "Error" => {
                if let Response::Error { code, description } = &resp {
                    match (&v["code"], code) {
                        (serde_json::Value::String(expected), Some(c)) => {
                            assert_eq!(c.as_str(), expected.as_str(), "error code for {line:?}");
                        }
                        (serde_json::Value::Null, None) => {}
                        _ => panic!("error code mismatch for {line:?}"),
                    }
                    assert_eq!(
                        description,
                        v["description"].as_str().unwrap(),
                        "error description for {line:?}"
                    );
                } else {
                    panic!("expected Error, got {resp:?}");
                }
            }
            _ => panic!("unknown response type: {resp_type}"),
        }
    }
}

#[test]
fn test_v3_frame_vectors() {
    let Some(vectors) = load_json("v3_frames") else {
        return;
    };

    for v in vectors.as_array().unwrap() {
        let frame_hex = v["frame_hex"].as_str().unwrap();
        let expected_seq = v["sequence"].as_u64().unwrap();
        let payload_hex = v["payload_hex"].as_str().unwrap();

        let frame_bytes = hex_decode(frame_hex);
        let expected_payload = hex_decode(payload_hex);

        let raw = v3::parse(&frame_bytes)
            .unwrap_or_else(|e| panic!("v3 parse failed for seq={expected_seq}: {e}"));

        assert_eq!(raw.sequence().value(), expected_seq);
        assert_eq!(raw.payload(), &expected_payload[..]);
    }
}

#[test]
fn test_v4_frame_vectors() {
    let Some(vectors) = load_json("v4_frames") else {
        return;
    };

    for v in vectors.as_array().unwrap() {
        let frame_hex = v["frame_hex"].as_str().unwrap();
        let expected_fmt = v["format_byte"].as_str().unwrap();
        let expected_subfmt = v["subformat_byte"].as_str().unwrap();
        let expected_seq = v["sequence"].as_u64().unwrap();
        let expected_station = v["station_id"].as_str().unwrap();
        let payload_hex = v["payload_hex"].as_str().unwrap();
        let expected_len = v["total_len"].as_u64().unwrap() as usize;

        let frame_bytes = hex_decode(frame_hex);
        let expected_payload = hex_decode(payload_hex);

        let (raw, consumed) = v4::parse(&frame_bytes)
            .unwrap_or_else(|e| panic!("v4 parse failed for station={expected_station}: {e}"));

        assert_eq!(consumed, expected_len);
        assert_eq!(raw.sequence().value(), expected_seq);
        assert_eq!(raw.payload(), &expected_payload[..]);

        match &raw {
            seedlink_rs_protocol::RawFrame::V4 {
                format,
                subformat,
                station_id,
                ..
            } => {
                let fmt_byte = format.to_byte();
                assert_eq!(
                    fmt_byte as char,
                    expected_fmt.chars().next().unwrap(),
                    "format mismatch"
                );
                let subfmt_byte = subformat.to_byte();
                assert_eq!(
                    subfmt_byte as char,
                    expected_subfmt.chars().next().unwrap(),
                    "subformat mismatch"
                );
                assert_eq!(*station_id, expected_station);
            }
            _ => panic!("expected V4 frame"),
        }
    }
}

/// Decode hex string to bytes.
fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
