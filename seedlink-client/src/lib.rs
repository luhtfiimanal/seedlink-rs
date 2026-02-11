//! Async SeedLink client for real-time seismic data streaming.
//!
//! Connect to SeedLink v3/v4 servers (IRIS, BMKG, GEOFON, etc.) and receive
//! miniSEED records in real-time over TCP.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> seedlink_rs_client::Result<()> {
//! use seedlink_rs_client::SeedLinkClient;
//!
//! let mut client = SeedLinkClient::connect("rtserve.iris.washington.edu:18000").await?;
//! client.station("ANMO", "IU").await?;
//! client.select("BHZ").await?;
//! client.data().await?;
//! client.end_stream().await?;
//!
//! while let Some(frame) = client.next_frame().await? {
//!     println!("seq={}, payload={} bytes", frame.sequence(), frame.payload().len());
//! }
//! # Ok(())
//! # }
//! ```

pub(crate) mod client;
pub(crate) mod connection;
pub(crate) mod error;
#[cfg(test)]
pub(crate) mod mock;
pub(crate) mod negotiate;
pub(crate) mod state;

pub use client::SeedLinkClient;
pub use error::{ClientError, Result};
pub use state::{ClientConfig, ClientState, OwnedFrame, ServerInfo, StationKey};
