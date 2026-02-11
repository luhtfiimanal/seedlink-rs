pub mod v3;
pub mod v4;

use crate::error::{Result, SeedlinkError};
use crate::sequence::SequenceNumber;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PayloadFormat {
    MiniSeed2,
    MiniSeed3,
    Json,
    Xml,
}

impl PayloadFormat {
    /// Parse from v4 format byte.
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            b'2' => Ok(Self::MiniSeed2),
            b'3' => Ok(Self::MiniSeed3),
            b'J' => Ok(Self::Json),
            b'X' => Ok(Self::Xml),
            _ => Err(SeedlinkError::InvalidPayloadFormat(b)),
        }
    }

    /// Serialize to v4 format byte.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::MiniSeed2 => b'2',
            Self::MiniSeed3 => b'3',
            Self::Json => b'J',
            Self::Xml => b'X',
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PayloadSubformat {
    Data,
    Event,
    Calibration,
    Timing,
    Log,
    Opaque,
    Info,
    InfoError,
}

impl PayloadSubformat {
    /// Parse from v4 subformat byte.
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            b'D' => Ok(Self::Data),
            b'E' => Ok(Self::Event),
            b'C' => Ok(Self::Calibration),
            b'T' => Ok(Self::Timing),
            b'L' => Ok(Self::Log),
            b'O' => Ok(Self::Opaque),
            b'I' => Ok(Self::Info),
            b'R' => Ok(Self::InfoError),
            _ => Err(SeedlinkError::InvalidPayloadSubformat(b)),
        }
    }

    /// Serialize to v4 subformat byte.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Data => b'D',
            Self::Event => b'E',
            Self::Calibration => b'C',
            Self::Timing => b'T',
            Self::Log => b'L',
            Self::Opaque => b'O',
            Self::Info => b'I',
            Self::InfoError => b'R',
        }
    }
}

/// Zero-copy frame — borrows payload from input buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawFrame<'a> {
    V3 {
        sequence: SequenceNumber,
        payload: &'a [u8],
    },
    V4 {
        format: PayloadFormat,
        subformat: PayloadSubformat,
        sequence: SequenceNumber,
        station_id: &'a str,
        payload: &'a [u8],
    },
}

impl<'a> RawFrame<'a> {
    pub fn sequence(&self) -> SequenceNumber {
        match self {
            Self::V3 { sequence, .. } | Self::V4 { sequence, .. } => *sequence,
        }
    }

    pub fn payload(&self) -> &'a [u8] {
        match self {
            Self::V3 { payload, .. } | Self::V4 { payload, .. } => payload,
        }
    }

    /// Decode the payload as a miniSEED record.
    pub fn decode(&self) -> Result<DataFrame> {
        let record = miniseed_rs::decode(self.payload())?;
        Ok(DataFrame {
            sequence: self.sequence(),
            record,
        })
    }
}

/// Owned frame with decoded miniSEED record.
#[derive(Debug)]
pub struct DataFrame {
    pub sequence: SequenceNumber,
    pub record: miniseed_rs::MseedRecord,
}
