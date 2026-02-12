#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] seedlink_rs_protocol::SeedlinkError),
    #[error("bind failed: {0}")]
    Bind(std::io::Error),
    #[error("invalid payload length: expected 512, got {0}")]
    InvalidPayloadLength(usize),
}

pub type Result<T> = std::result::Result<T, ServerError>;
