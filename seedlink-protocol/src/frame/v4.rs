use crate::error::{Result, SeedlinkError};
use crate::frame::{PayloadFormat, PayloadSubformat, RawFrame};
use crate::sequence::SequenceNumber;

pub const SIGNATURE: &[u8; 2] = b"SE";

/// Minimum header size: 2 (sig) + 1 (format) + 1 (subformat) + 4 (payload len)
///                    + 8 (sequence) + 1 (station id len) = 17
pub const MIN_HEADER_LEN: usize = 17;

/// Parse a v4 frame from the beginning of a buffer.
///
/// Returns `(frame, bytes_consumed)` because v4 frames are variable-length.
pub fn parse(data: &[u8]) -> Result<(RawFrame<'_>, usize)> {
    if data.len() < MIN_HEADER_LEN {
        return Err(SeedlinkError::FrameTooShort {
            expected: MIN_HEADER_LEN,
            actual: data.len(),
        });
    }

    // Check signature
    if &data[0..2] != SIGNATURE.as_slice() {
        return Err(SeedlinkError::InvalidSignature {
            expected: "SE",
            actual: [data[0], data[1]],
        });
    }

    let format = PayloadFormat::from_byte(data[2])?;
    let subformat = PayloadSubformat::from_byte(data[3])?;

    let payload_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

    let seq_bytes: [u8; 8] = data[8..16].try_into().unwrap();
    let sequence = SequenceNumber::from_v4_le_bytes(seq_bytes);

    let station_id_len = data[16] as usize;
    let header_len = MIN_HEADER_LEN + station_id_len;
    let total_len = header_len + payload_len;

    if data.len() < total_len {
        return Err(SeedlinkError::FrameTooShort {
            expected: total_len,
            actual: data.len(),
        });
    }

    let station_id = std::str::from_utf8(&data[17..17 + station_id_len])
        .map_err(|_| SeedlinkError::InvalidCommand("station ID is not valid UTF-8".into()))?;

    let payload = &data[header_len..total_len];

    Ok((
        RawFrame::V4 {
            format,
            subformat,
            sequence,
            station_id,
            payload,
        },
        total_len,
    ))
}

/// Write a v4 frame.
pub fn write(
    format: PayloadFormat,
    subformat: PayloadSubformat,
    sequence: SequenceNumber,
    station_id: &str,
    payload: &[u8],
) -> Result<Vec<u8>> {
    let station_id_bytes = station_id.as_bytes();
    let station_id_len = station_id_bytes.len();

    let header_len = MIN_HEADER_LEN + station_id_len;
    let total_len = header_len + payload.len();

    let mut frame = Vec::with_capacity(total_len);

    // Signature
    frame.extend_from_slice(SIGNATURE);
    // Format + Subformat
    frame.push(format.to_byte());
    frame.push(subformat.to_byte());
    // Payload length (u32 LE)
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    // Sequence (u64 LE)
    frame.extend_from_slice(&sequence.to_v4_le_bytes());
    // Station ID length (u8) + Station ID
    frame.push(station_id_len as u8);
    frame.extend_from_slice(station_id_bytes);
    // Payload
    frame.extend_from_slice(payload);

    debug_assert_eq!(frame.len(), total_len);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_parse_roundtrip() {
        let payload = b"test payload data for v4 frame";
        let seq = SequenceNumber::new(42);

        let frame = write(
            PayloadFormat::MiniSeed2,
            PayloadSubformat::Data,
            seq,
            "IU_ANMO",
            payload,
        )
        .unwrap();

        let (parsed, consumed) = parse(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(parsed.sequence(), seq);
        assert_eq!(parsed.payload(), payload);

        match &parsed {
            RawFrame::V4 {
                format,
                subformat,
                station_id,
                ..
            } => {
                assert_eq!(*format, PayloadFormat::MiniSeed2);
                assert_eq!(*subformat, PayloadSubformat::Data);
                assert_eq!(*station_id, "IU_ANMO");
            }
            _ => panic!("expected V4 frame"),
        }
    }

    #[test]
    fn all_format_subformat_combos() {
        let formats = [
            PayloadFormat::MiniSeed2,
            PayloadFormat::MiniSeed3,
            PayloadFormat::Json,
            PayloadFormat::Xml,
        ];
        let subformats = [
            PayloadSubformat::Data,
            PayloadSubformat::Event,
            PayloadSubformat::Calibration,
            PayloadSubformat::Timing,
            PayloadSubformat::Log,
            PayloadSubformat::Opaque,
            PayloadSubformat::Info,
            PayloadSubformat::InfoError,
        ];

        let payload = b"hello";
        for fmt in &formats {
            for subfmt in &subformats {
                let frame = write(*fmt, *subfmt, SequenceNumber::new(1), "X", payload).unwrap();
                let (parsed, _) = parse(&frame).unwrap();
                match parsed {
                    RawFrame::V4 {
                        format, subformat, ..
                    } => {
                        assert_eq!(format, *fmt);
                        assert_eq!(subformat, *subfmt);
                    }
                    _ => panic!("expected V4"),
                }
            }
        }
    }

    #[test]
    fn empty_station_id() {
        let payload = b"data";
        let frame = write(
            PayloadFormat::Json,
            PayloadSubformat::Info,
            SequenceNumber::new(0),
            "",
            payload,
        )
        .unwrap();

        let (parsed, consumed) = parse(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        match parsed {
            RawFrame::V4 { station_id, .. } => assert_eq!(station_id, ""),
            _ => panic!("expected V4"),
        }
    }

    #[test]
    fn long_station_id() {
        let station = "XFDSN_IU_ANMO_00_BHZ";
        let payload = b"data";
        let frame = write(
            PayloadFormat::MiniSeed3,
            PayloadSubformat::Data,
            SequenceNumber::new(999),
            station,
            payload,
        )
        .unwrap();

        let (parsed, _) = parse(&frame).unwrap();
        match parsed {
            RawFrame::V4 { station_id, .. } => assert_eq!(station_id, station),
            _ => panic!("expected V4"),
        }
    }

    #[test]
    fn parse_wrong_signature() {
        let frame = write(
            PayloadFormat::MiniSeed2,
            PayloadSubformat::Data,
            SequenceNumber::new(0),
            "",
            b"data",
        )
        .unwrap();

        let mut bad = frame.clone();
        bad[0] = b'X';
        bad[1] = b'Y';
        assert!(matches!(
            parse(&bad).unwrap_err(),
            SeedlinkError::InvalidSignature { .. }
        ));
    }

    #[test]
    fn parse_truncated() {
        let frame = write(
            PayloadFormat::MiniSeed2,
            PayloadSubformat::Data,
            SequenceNumber::new(0),
            "IU_ANMO",
            b"some payload data",
        )
        .unwrap();

        // Cut off partway through payload
        let truncated = &frame[..frame.len() - 5];
        assert!(matches!(
            parse(truncated).unwrap_err(),
            SeedlinkError::FrameTooShort { .. }
        ));
    }

    #[test]
    fn parse_too_short_for_header() {
        assert!(matches!(
            parse(&[0u8; 5]).unwrap_err(),
            SeedlinkError::FrameTooShort { .. }
        ));
    }

    #[test]
    fn invalid_format_byte() {
        assert!(matches!(
            PayloadFormat::from_byte(b'Z').unwrap_err(),
            SeedlinkError::InvalidPayloadFormat(b'Z')
        ));
    }

    #[test]
    fn invalid_subformat_byte() {
        assert!(matches!(
            PayloadSubformat::from_byte(b'Z').unwrap_err(),
            SeedlinkError::InvalidPayloadSubformat(b'Z')
        ));
    }

    #[test]
    fn empty_payload() {
        let frame = write(
            PayloadFormat::Json,
            PayloadSubformat::Info,
            SequenceNumber::new(0),
            "",
            b"",
        )
        .unwrap();

        let (parsed, consumed) = parse(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(parsed.payload(), b"");
    }

    #[test]
    fn large_payload() {
        let payload = vec![0xAA_u8; 4096];
        let frame = write(
            PayloadFormat::MiniSeed3,
            PayloadSubformat::Data,
            SequenceNumber::new(u64::MAX - 2),
            "NET_STA",
            &payload,
        )
        .unwrap();

        let (parsed, consumed) = parse(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(parsed.payload().len(), 4096);
    }

    #[test]
    fn format_byte_roundtrip() {
        let formats = [
            PayloadFormat::MiniSeed2,
            PayloadFormat::MiniSeed3,
            PayloadFormat::Json,
            PayloadFormat::Xml,
        ];
        for fmt in formats {
            assert_eq!(PayloadFormat::from_byte(fmt.to_byte()).unwrap(), fmt);
        }
    }

    #[test]
    fn subformat_byte_roundtrip() {
        let subformats = [
            PayloadSubformat::Data,
            PayloadSubformat::Event,
            PayloadSubformat::Calibration,
            PayloadSubformat::Timing,
            PayloadSubformat::Log,
            PayloadSubformat::Opaque,
            PayloadSubformat::Info,
            PayloadSubformat::InfoError,
        ];
        for subfmt in subformats {
            assert_eq!(
                PayloadSubformat::from_byte(subfmt.to_byte()).unwrap(),
                subfmt
            );
        }
    }
}
