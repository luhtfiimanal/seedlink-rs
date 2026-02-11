use crate::error::{Result, SeedlinkError};
use crate::frame::RawFrame;
use crate::sequence::SequenceNumber;

pub const SIGNATURE: &[u8; 2] = b"SL";
pub const HEADER_LEN: usize = 8;
pub const PAYLOAD_LEN: usize = 512;
pub const FRAME_LEN: usize = 520;

/// Parse a v3 frame from exactly 520 bytes.
pub fn parse(data: &[u8]) -> Result<RawFrame<'_>> {
    if data.len() < FRAME_LEN {
        return Err(SeedlinkError::FrameTooShort {
            expected: FRAME_LEN,
            actual: data.len(),
        });
    }

    // Check signature
    if &data[0..2] != SIGNATURE.as_slice() {
        return Err(SeedlinkError::InvalidSignature {
            expected: "SL",
            actual: [data[0], data[1]],
        });
    }

    // Parse sequence number from 6 hex ASCII chars at bytes 2..8
    let hex_str = std::str::from_utf8(&data[2..8])
        .map_err(|_| SeedlinkError::InvalidSequence("sequence bytes are not valid UTF-8".into()))?;
    let sequence = SequenceNumber::from_v3_hex(hex_str)?;

    let payload = &data[HEADER_LEN..FRAME_LEN];

    Ok(RawFrame::V3 { sequence, payload })
}

/// Write a v3 frame (520 bytes) from sequence number and payload.
pub fn write(sequence: SequenceNumber, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() != PAYLOAD_LEN {
        return Err(SeedlinkError::PayloadLengthMismatch {
            expected: PAYLOAD_LEN,
            actual: payload.len(),
        });
    }

    let mut frame = Vec::with_capacity(FRAME_LEN);
    frame.extend_from_slice(SIGNATURE);
    frame.extend_from_slice(sequence.to_v3_hex().as_bytes());
    frame.extend_from_slice(payload);

    debug_assert_eq!(frame.len(), FRAME_LEN);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_frame(seq_hex: &str, payload: &[u8; PAYLOAD_LEN]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(FRAME_LEN);
        frame.extend_from_slice(b"SL");
        frame.extend_from_slice(seq_hex.as_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn parse_valid() {
        let payload = [0xAA_u8; PAYLOAD_LEN];
        let frame = make_test_frame("00001A", &payload);

        let raw = parse(&frame).unwrap();
        assert_eq!(raw.sequence(), SequenceNumber::new(26));
        assert_eq!(raw.payload(), &payload[..]);
    }

    #[test]
    fn parse_wrong_signature() {
        let payload = [0u8; PAYLOAD_LEN];
        let mut frame = make_test_frame("000001", &payload);
        frame[0] = b'X';
        frame[1] = b'Y';

        let err = parse(&frame).unwrap_err();
        assert!(matches!(err, SeedlinkError::InvalidSignature { .. }));
    }

    #[test]
    fn parse_too_short() {
        let data = b"SL00001A";
        let err = parse(data).unwrap_err();
        assert!(matches!(err, SeedlinkError::FrameTooShort { .. }));
    }

    #[test]
    fn parse_empty() {
        let err = parse(&[]).unwrap_err();
        assert!(matches!(err, SeedlinkError::FrameTooShort { .. }));
    }

    #[test]
    fn write_valid() {
        let payload = [0x42_u8; PAYLOAD_LEN];
        let seq = SequenceNumber::new(0xFF);

        let frame = write(seq, &payload).unwrap();
        assert_eq!(frame.len(), FRAME_LEN);
        assert_eq!(&frame[0..2], b"SL");
        assert_eq!(&frame[2..8], b"0000FF");
        assert_eq!(&frame[8..], &payload[..]);
    }

    #[test]
    fn write_wrong_payload_size() {
        let payload = [0u8; 100];
        let err = write(SequenceNumber::new(0), &payload).unwrap_err();
        assert!(matches!(err, SeedlinkError::PayloadLengthMismatch { .. }));
    }

    #[test]
    fn write_parse_roundtrip() {
        let seq = SequenceNumber::new(0xABCDEF);
        let payload = [0x55_u8; PAYLOAD_LEN];

        let frame = write(seq, &payload).unwrap();
        let parsed = parse(&frame).unwrap();

        assert_eq!(parsed.sequence(), seq);
        assert_eq!(parsed.payload(), &payload[..]);
    }

    #[test]
    fn parse_boundary_sequences() {
        // Zero
        let payload = [0u8; PAYLOAD_LEN];
        let frame = make_test_frame("000000", &payload);
        let raw = parse(&frame).unwrap();
        assert_eq!(raw.sequence(), SequenceNumber::new(0));

        // Max v3
        let frame = make_test_frame("FFFFFF", &payload);
        let raw = parse(&frame).unwrap();
        assert_eq!(raw.sequence(), SequenceNumber::new(0xFFFFFF));
    }
}
