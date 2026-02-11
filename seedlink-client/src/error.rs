use std::time::Duration;

/// Errors that can occur during SeedLink client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// TCP or socket I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// SeedLink protocol parsing error (invalid frame, bad command format, etc.).
    #[error("protocol error: {0}")]
    Protocol(#[from] seedlink_rs_protocol::SeedlinkError),

    /// Operation exceeded the configured timeout duration.
    #[error("timeout after {0:?}")]
    Timeout(Duration),

    /// Server closed the connection (read returned 0 bytes).
    #[error("disconnected")]
    Disconnected,

    /// Server returned an ERROR response to a command.
    #[error("server error: {0}")]
    ServerError(String),

    /// Method called in wrong client state (e.g., `next_frame` before `end_stream`).
    #[error("invalid state: expected {expected}, actual {actual}")]
    InvalidState {
        /// The state(s) required for the operation.
        expected: &'static str,
        /// The current client state.
        actual: &'static str,
    },

    /// Protocol version negotiation failed.
    #[error("negotiation failed: {0}")]
    NegotiationFailed(String),

    /// Server sent an unexpected response line.
    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}

/// Convenience alias for `Result<T, ClientError>`.
pub type Result<T> = std::result::Result<T, ClientError>;
