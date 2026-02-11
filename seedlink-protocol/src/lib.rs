//! SeedLink protocol types, commands, and frame parsing.
//!
//! This crate provides the shared protocol layer for SeedLink v3/v4,
//! used by both the client and server crates.

pub mod command;
pub mod error;
pub mod frame;
pub mod info;
pub mod response;
pub mod sequence;
pub mod version;

pub use command::Command;
pub use error::{Result, SeedlinkError};
pub use frame::{DataFrame, PayloadFormat, PayloadSubformat, RawFrame};
pub use info::InfoLevel;
pub use response::Response;
pub use sequence::SequenceNumber;
pub use version::ProtocolVersion;
