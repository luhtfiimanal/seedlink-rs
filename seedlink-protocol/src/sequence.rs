use crate::error::{Result, SeedlinkError};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SequenceNumber(u64);

impl SequenceNumber {
    /// Sentinel: sequence not set.
    pub const UNSET: Self = Self(u64::MAX);

    /// Sentinel: request all data (v4).
    pub const ALL_DATA: Self = Self(u64::MAX - 1);

    /// Maximum sequence value for v3 (6 hex digits).
    pub const V3_MAX: u64 = 0xFF_FFFF;

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// Returns true if this is a special sentinel value (UNSET or ALL_DATA).
    pub fn is_special(self) -> bool {
        self == Self::UNSET || self == Self::ALL_DATA
    }

    /// Parse v3 hex representation (6 uppercase hex digits, e.g. "00001A").
    pub fn from_v3_hex(hex: &str) -> Result<Self> {
        if hex.len() != 6 {
            return Err(SeedlinkError::InvalidSequence(format!(
                "v3 hex must be 6 chars, got {} ({hex:?})",
                hex.len()
            )));
        }
        let value = u64::from_str_radix(hex, 16)
            .map_err(|_| SeedlinkError::InvalidSequence(format!("invalid v3 hex: {hex:?}")))?;
        Ok(Self(value))
    }

    /// Serialize to v3 hex (6 uppercase hex digits).
    pub fn to_v3_hex(self) -> String {
        format!("{:06X}", self.0)
    }

    /// Parse v4 decimal string (e.g. "26").
    pub fn from_v4_decimal(s: &str) -> Result<Self> {
        let value: u64 = s
            .parse()
            .map_err(|_| SeedlinkError::InvalidSequence(format!("invalid v4 decimal: {s:?}")))?;
        Ok(Self(value))
    }

    /// Serialize to v4 decimal string.
    pub fn to_v4_decimal(self) -> String {
        self.0.to_string()
    }

    /// Parse from v4 little-endian bytes (frame header).
    pub fn from_v4_le_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_le_bytes(bytes))
    }

    /// Serialize to v4 little-endian bytes (frame header).
    pub fn to_v4_le_bytes(self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

impl PartialOrd for SequenceNumber {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SequenceNumber {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl std::fmt::Display for SequenceNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == Self::UNSET {
            write!(f, "UNSET")
        } else if *self == Self::ALL_DATA {
            write!(f, "ALL_DATA")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_hex_valid() {
        let seq = SequenceNumber::from_v3_hex("00001A").unwrap();
        assert_eq!(seq.value(), 26);
        assert_eq!(seq.to_v3_hex(), "00001A");
    }

    #[test]
    fn v3_hex_boundary_zero() {
        let seq = SequenceNumber::from_v3_hex("000000").unwrap();
        assert_eq!(seq.value(), 0);
        assert_eq!(seq.to_v3_hex(), "000000");
    }

    #[test]
    fn v3_hex_boundary_max() {
        let seq = SequenceNumber::from_v3_hex("FFFFFF").unwrap();
        assert_eq!(seq.value(), 0xFFFFFF);
        assert_eq!(seq.to_v3_hex(), "FFFFFF");
    }

    #[test]
    fn v3_hex_lowercase_accepted() {
        let seq = SequenceNumber::from_v3_hex("00001a").unwrap();
        assert_eq!(seq.value(), 26);
    }

    #[test]
    fn v3_hex_invalid_chars() {
        assert!(SequenceNumber::from_v3_hex("ZZZZZZ").is_err());
    }

    #[test]
    fn v3_hex_wrong_length() {
        assert!(SequenceNumber::from_v3_hex("001A").is_err());
        assert!(SequenceNumber::from_v3_hex("0000001A").is_err());
    }

    #[test]
    fn v3_hex_roundtrip() {
        for val in [0u64, 1, 255, 0xFFFFFF] {
            let seq = SequenceNumber::new(val);
            let hex = seq.to_v3_hex();
            let parsed = SequenceNumber::from_v3_hex(&hex).unwrap();
            assert_eq!(parsed, seq);
        }
    }

    #[test]
    fn v4_decimal_valid() {
        let seq = SequenceNumber::from_v4_decimal("26").unwrap();
        assert_eq!(seq.value(), 26);
        assert_eq!(seq.to_v4_decimal(), "26");
    }

    #[test]
    fn v4_decimal_zero() {
        let seq = SequenceNumber::from_v4_decimal("0").unwrap();
        assert_eq!(seq.value(), 0);
    }

    #[test]
    fn v4_decimal_invalid() {
        assert!(SequenceNumber::from_v4_decimal("abc").is_err());
        assert!(SequenceNumber::from_v4_decimal("-1").is_err());
    }

    #[test]
    fn v4_le_bytes_roundtrip() {
        let seq = SequenceNumber::new(0x0102030405060708);
        let bytes = seq.to_v4_le_bytes();
        assert_eq!(SequenceNumber::from_v4_le_bytes(bytes), seq);
    }

    #[test]
    fn ordering() {
        let a = SequenceNumber::new(10);
        let b = SequenceNumber::new(20);
        assert!(a < b);
        assert!(b > a);
    }

    #[test]
    fn special_values() {
        assert!(SequenceNumber::UNSET.is_special());
        assert!(SequenceNumber::ALL_DATA.is_special());
        assert!(!SequenceNumber::new(0).is_special());
        assert!(!SequenceNumber::new(42).is_special());
    }

    #[test]
    fn display_special() {
        assert_eq!(SequenceNumber::UNSET.to_string(), "UNSET");
        assert_eq!(SequenceNumber::ALL_DATA.to_string(), "ALL_DATA");
        assert_eq!(SequenceNumber::new(42).to_string(), "42");
    }
}
