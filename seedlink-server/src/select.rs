/// SELECT pattern parsing and matching for SeedLink v3.
///
/// Pattern format: `[LL]CCC[.T]`
/// - LL = 2-char location code (optional)
/// - CCC = 3-char channel code (required)
/// - .T = type/quality code suffix (optional)
/// - `?` is single-char wildcard

#[derive(Clone, Debug)]
enum PatternChar {
    Literal(u8),
    Wildcard,
}

impl PatternChar {
    fn matches(&self, byte: u8) -> bool {
        match self {
            PatternChar::Literal(b) => *b == byte,
            PatternChar::Wildcard => true,
        }
    }

    fn from_byte(b: u8) -> Self {
        if b == b'?' {
            PatternChar::Wildcard
        } else {
            PatternChar::Literal(b)
        }
    }
}

/// A parsed SELECT pattern.
#[derive(Clone, Debug)]
pub(crate) struct SelectPattern {
    location: Option<[PatternChar; 2]>,
    channel: [PatternChar; 3],
    type_code: Option<u8>,
}

impl SelectPattern {
    /// Parse a SELECT pattern string.
    ///
    /// Format: `[LL]CCC[.T]` — NO dot between location and channel.
    pub fn parse(pattern: &str) -> Option<Self> {
        if pattern.is_empty() {
            return None;
        }

        let bytes = pattern.as_bytes();

        // 1. Strip `.T` suffix if present
        let (main, type_code) = if bytes.len() >= 2 && bytes[bytes.len() - 2] == b'.' {
            let tc = bytes[bytes.len() - 1];
            (&bytes[..bytes.len() - 2], Some(tc))
        } else {
            (bytes, None)
        };

        // 2. Parse location + channel from remaining
        let (location, channel) = match main.len() {
            0 => return None,
            1 => {
                // Pad left to 3 chars: "Z" → "??Z"
                (
                    None,
                    [
                        PatternChar::Wildcard,
                        PatternChar::Wildcard,
                        PatternChar::from_byte(main[0]),
                    ],
                )
            }
            2 => {
                // Pad left to 3 chars: "HZ" → "?HZ"
                (
                    None,
                    [
                        PatternChar::Wildcard,
                        PatternChar::from_byte(main[0]),
                        PatternChar::from_byte(main[1]),
                    ],
                )
            }
            3 => {
                // Channel only
                (
                    None,
                    [
                        PatternChar::from_byte(main[0]),
                        PatternChar::from_byte(main[1]),
                        PatternChar::from_byte(main[2]),
                    ],
                )
            }
            5 => {
                // Location (2) + Channel (3)
                let loc = [
                    PatternChar::from_byte(main[0]),
                    PatternChar::from_byte(main[1]),
                ];
                let ch = [
                    PatternChar::from_byte(main[2]),
                    PatternChar::from_byte(main[3]),
                    PatternChar::from_byte(main[4]),
                ];
                (Some(loc), ch)
            }
            _ => {
                // len == 4 or len > 5: take last 3 as channel, rest as location
                if main.len() < 3 {
                    return None;
                }
                let split = main.len() - 3;
                let loc_bytes = &main[..split];
                let ch_bytes = &main[split..];
                let loc = if loc_bytes.len() >= 2 {
                    [
                        PatternChar::from_byte(loc_bytes[0]),
                        PatternChar::from_byte(loc_bytes[1]),
                    ]
                } else {
                    [PatternChar::Wildcard, PatternChar::from_byte(loc_bytes[0])]
                };
                let ch = [
                    PatternChar::from_byte(ch_bytes[0]),
                    PatternChar::from_byte(ch_bytes[1]),
                    PatternChar::from_byte(ch_bytes[2]),
                ];
                (Some(loc), ch)
            }
        };

        Some(Self {
            location,
            channel,
            type_code,
        })
    }

    /// Check if this pattern matches a miniSEED v2 payload.
    ///
    /// miniSEED v2 fixed header offsets:
    /// - byte 6: quality/type indicator
    /// - bytes 13..15: location (2 chars)
    /// - bytes 15..18: channel (3 chars)
    pub fn matches_payload(&self, payload: &[u8]) -> bool {
        if payload.len() < 20 {
            return false;
        }

        // Match channel (always required)
        if !self.channel[0].matches(payload[15])
            || !self.channel[1].matches(payload[16])
            || !self.channel[2].matches(payload[17])
        {
            return false;
        }

        // Match location (only if pattern specifies it)
        if let Some(ref loc) = self.location
            && (!loc[0].matches(payload[13]) || !loc[1].matches(payload[14]))
        {
            return false;
        }

        // Match type code (only if pattern specifies .T suffix)
        if let Some(tc) = self.type_code {
            if PatternChar::from_byte(tc).matches(payload[6]) {
                // match
            } else {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mseed_payload(location: &[u8; 2], channel: &[u8; 3], quality: u8) -> Vec<u8> {
        let mut payload = vec![0u8; 512];
        payload[6] = quality;
        payload[13] = location[0];
        payload[14] = location[1];
        payload[15] = channel[0];
        payload[16] = channel[1];
        payload[17] = channel[2];
        payload
    }

    #[test]
    fn parse_channel_only() {
        let pat = SelectPattern::parse("BHZ").unwrap();
        assert!(pat.location.is_none());
        assert!(pat.type_code.is_none());

        let payload = make_mseed_payload(b"00", b"BHZ", b'D');
        assert!(pat.matches_payload(&payload));

        let payload2 = make_mseed_payload(b"00", b"BHN", b'D');
        assert!(!pat.matches_payload(&payload2));
    }

    #[test]
    fn parse_location_channel() {
        let pat = SelectPattern::parse("00BHZ").unwrap();
        assert!(pat.location.is_some());

        let payload = make_mseed_payload(b"00", b"BHZ", b'D');
        assert!(pat.matches_payload(&payload));

        // Different location
        let payload2 = make_mseed_payload(b"10", b"BHZ", b'D');
        assert!(!pat.matches_payload(&payload2));
    }

    #[test]
    fn parse_with_type_suffix() {
        let pat = SelectPattern::parse("BHZ.D").unwrap();
        assert!(pat.type_code.is_some());

        let payload = make_mseed_payload(b"00", b"BHZ", b'D');
        assert!(pat.matches_payload(&payload));

        let payload2 = make_mseed_payload(b"00", b"BHZ", b'R');
        assert!(!pat.matches_payload(&payload2));
    }

    #[test]
    fn wildcard_channel() {
        let pat = SelectPattern::parse("BH?").unwrap();

        let bhz = make_mseed_payload(b"00", b"BHZ", b'D');
        let bhn = make_mseed_payload(b"00", b"BHN", b'D');
        let bhe = make_mseed_payload(b"00", b"BHE", b'D');
        let lhz = make_mseed_payload(b"00", b"LHZ", b'D');

        assert!(pat.matches_payload(&bhz));
        assert!(pat.matches_payload(&bhn));
        assert!(pat.matches_payload(&bhe));
        assert!(!pat.matches_payload(&lhz));
    }

    #[test]
    fn wildcard_location() {
        let pat = SelectPattern::parse("??BHZ").unwrap();
        assert!(pat.location.is_some());

        let payload00 = make_mseed_payload(b"00", b"BHZ", b'D');
        let payload10 = make_mseed_payload(b"10", b"BHZ", b'D');

        assert!(pat.matches_payload(&payload00));
        assert!(pat.matches_payload(&payload10));
    }

    #[test]
    fn short_payload_returns_false() {
        let pat = SelectPattern::parse("BHZ").unwrap();
        assert!(!pat.matches_payload(&[0u8; 10]));
    }

    #[test]
    fn empty_pattern_returns_none() {
        assert!(SelectPattern::parse("").is_none());
    }

    #[test]
    fn full_pattern_with_location_and_type() {
        let pat = SelectPattern::parse("00BHZ.D").unwrap();
        assert!(pat.location.is_some());
        assert!(pat.type_code.is_some());

        let payload = make_mseed_payload(b"00", b"BHZ", b'D');
        assert!(pat.matches_payload(&payload));

        // Wrong location
        let payload2 = make_mseed_payload(b"10", b"BHZ", b'D');
        assert!(!pat.matches_payload(&payload2));

        // Wrong type
        let payload3 = make_mseed_payload(b"00", b"BHZ", b'R');
        assert!(!pat.matches_payload(&payload3));
    }

    #[test]
    fn single_char_padded() {
        // "Z" → matches any channel ending in Z
        let pat = SelectPattern::parse("Z").unwrap();
        let bhz = make_mseed_payload(b"00", b"BHZ", b'D');
        let bhn = make_mseed_payload(b"00", b"BHN", b'D');
        assert!(pat.matches_payload(&bhz));
        assert!(!pat.matches_payload(&bhn));
    }
}
