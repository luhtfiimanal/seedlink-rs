//! Tests that verify Python-captured conversation vectors parse correctly.
//!
//! These tests load JSON from `pyscripts/test_vectors/` and verify that
//! protocol types parse the captured data correctly.
//!
//! If the JSON files don't exist (not generated yet), tests are skipped.

use std::path::Path;

use seedlink_rs_protocol::frame::{v3, v4};
use seedlink_rs_protocol::{Response, SequenceNumber};
use serde::Deserialize;

const VECTORS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../pyscripts/test_vectors");

#[derive(Deserialize)]
struct V3Conversation {
    hello_line1: String,
    hello_line2: String,
    frames: Vec<V3FrameVector>,
}

#[derive(Deserialize)]
struct V3FrameVector {
    sequence_hex: String,
    sequence: u64,
    frame_hex: String,
    ms_station: String,
    ms_network: String,
}

#[derive(Deserialize)]
struct V4Conversation {
    hello_line1: String,
    hello_line2: String,
    frames: Vec<V4FrameVector>,
}

#[derive(Deserialize)]
struct V4FrameVector {
    sequence: u64,
    station_id: String,
    frame_hex: String,
    payload_len: usize,
}

/// Verify v3 conversation: HELLO parsing and frame parsing.
#[test]
fn verify_v3_conversation() {
    let path = Path::new(VECTORS_DIR).join("client_v3_conversation.json");
    if !path.exists() {
        eprintln!("skipping: {path:?} not found (run capture_client_vectors.py first)");
        return;
    }

    let data = std::fs::read_to_string(&path).unwrap();
    let conv: V3Conversation = serde_json::from_str(&data).unwrap();

    // Verify HELLO parses
    let hello = Response::parse_hello(&conv.hello_line1, &conv.hello_line2).unwrap();
    match &hello {
        Response::Hello {
            software,
            organization,
            ..
        } => {
            assert!(!software.is_empty());
            assert!(!organization.is_empty());
        }
        _ => panic!("expected Hello response"),
    }

    // Verify each frame parses correctly
    for (i, fv) in conv.frames.iter().enumerate() {
        let frame_bytes = hex::decode(&fv.frame_hex).unwrap();
        assert_eq!(frame_bytes.len(), v3::FRAME_LEN, "frame {i} wrong length");

        let raw = v3::parse(&frame_bytes).unwrap();
        assert_eq!(
            raw.sequence(),
            SequenceNumber::new(fv.sequence),
            "frame {i} sequence mismatch"
        );

        // Verify station/network extraction from miniSEED v2 header
        let payload = raw.payload();
        if payload.len() >= 20 {
            let ms_station = std::str::from_utf8(&payload[8..13]).unwrap().trim();
            let ms_network = std::str::from_utf8(&payload[18..20]).unwrap().trim();
            assert_eq!(ms_station, fv.ms_station, "frame {i} station mismatch");
            assert_eq!(ms_network, fv.ms_network, "frame {i} network mismatch");
        }

        // Verify sequence hex matches
        assert_eq!(
            SequenceNumber::new(fv.sequence).to_v3_hex(),
            fv.sequence_hex,
            "frame {i} hex mismatch"
        );
    }
}

/// Verify v4 conversation: frame parsing.
#[test]
fn verify_v4_conversation() {
    let path = Path::new(VECTORS_DIR).join("client_v4_conversation.json");
    if !path.exists() {
        eprintln!("skipping: {path:?} not found (run capture_client_vectors.py first)");
        return;
    }

    let data = std::fs::read_to_string(&path).unwrap();
    let conv: V4Conversation = serde_json::from_str(&data).unwrap();

    // Verify HELLO parses
    let hello = Response::parse_hello(&conv.hello_line1, &conv.hello_line2).unwrap();
    match &hello {
        Response::Hello { software, .. } => {
            assert!(!software.is_empty());
        }
        _ => panic!("expected Hello response"),
    }

    // Verify each frame
    for (i, fv) in conv.frames.iter().enumerate() {
        let frame_bytes = hex::decode(&fv.frame_hex).unwrap();
        let (raw, consumed) = v4::parse(&frame_bytes).unwrap();
        assert_eq!(consumed, frame_bytes.len(), "frame {i} consumed mismatch");
        assert_eq!(
            raw.sequence(),
            SequenceNumber::new(fv.sequence),
            "frame {i} sequence mismatch"
        );
        assert_eq!(
            raw.payload().len(),
            fv.payload_len,
            "frame {i} payload length mismatch"
        );
        match &raw {
            seedlink_rs_protocol::RawFrame::V4 { station_id, .. } => {
                assert_eq!(*station_id, fv.station_id, "frame {i} station_id mismatch");
            }
            _ => panic!("expected V4 frame at index {i}"),
        }
    }
}

/// Verify HELLO response parsing against captured data from multiple servers.
#[test]
fn verify_hello_responses() {
    let path = Path::new(VECTORS_DIR).join("client_hello_responses.json");
    if !path.exists() {
        eprintln!("skipping: {path:?} not found");
        return;
    }

    #[derive(Deserialize)]
    struct HelloEntry {
        server: String,
        line1: String,
        line2: String,
    }

    let data = std::fs::read_to_string(&path).unwrap();
    let entries: Vec<HelloEntry> = serde_json::from_str(&data).unwrap();

    for entry in &entries {
        let resp = Response::parse_hello(&entry.line1, &entry.line2).unwrap();
        match resp {
            Response::Hello {
                software,
                organization,
                ..
            } => {
                assert!(!software.is_empty(), "empty software for {}", entry.server);
                assert!(
                    !organization.is_empty(),
                    "empty organization for {}",
                    entry.server
                );
            }
            _ => panic!("expected Hello response for {}", entry.server),
        }
    }
}
