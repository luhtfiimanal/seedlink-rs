#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] seedlink_rs_protocol::SeedlinkError),
    #[error("bind failed: {0}")]
    Bind(std::io::Error),
    #[error("payload is empty")]
    EmptyPayload,
    #[error("payload too large: {len} bytes (max {max})")]
    PayloadTooLarge { len: usize, max: usize },
    #[error("journal error: {0}")]
    Journal(std::io::Error),
}

pub type Result<T> = std::result::Result<T, ServerError>;
