use crate::version::ProtocolVersion;

#[derive(Debug, thiserror::Error)]
pub enum SeedlinkError {
    #[error("frame too short: expected {expected}, actual {actual}")]
    FrameTooShort { expected: usize, actual: usize },

    #[error("invalid signature: expected {expected:?}, actual {actual:?}")]
    InvalidSignature {
        expected: &'static str,
        actual: [u8; 2],
    },

    #[error("invalid sequence: {0}")]
    InvalidSequence(String),

    #[error("invalid command: {0}")]
    InvalidCommand(String),

    #[error("version mismatch: {command} not valid for {version:?}")]
    VersionMismatch {
        command: &'static str,
        version: ProtocolVersion,
    },

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("server error: [{code}] {description}")]
    ServerError { code: String, description: String },

    #[error("invalid info level: {0}")]
    InvalidInfoLevel(String),

    #[error("invalid payload format: {0}")]
    InvalidPayloadFormat(u8),

    #[error("invalid payload subformat: {0}")]
    InvalidPayloadSubformat(u8),

    #[error("payload length mismatch: expected {expected}, actual {actual}")]
    PayloadLengthMismatch { expected: usize, actual: usize },

    #[error("miniseed error: {0}")]
    Miniseed(#[from] miniseed_rs::MseedError),
}

pub type Result<T> = std::result::Result<T, SeedlinkError>;
