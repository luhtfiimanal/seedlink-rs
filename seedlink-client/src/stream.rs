use futures_core::Stream;

use crate::SeedLinkClient;
use crate::error::ClientError;
use crate::state::OwnedFrame;

/// Convert a streaming [`SeedLinkClient`] into a [`Stream`] of frames.
///
/// The client must be in the `Streaming` state (i.e., after calling
/// [`end_stream()`](SeedLinkClient::end_stream) or [`fetch()`](SeedLinkClient::fetch)).
///
/// The stream yields `Ok(OwnedFrame)` for each received frame and terminates
/// with `None` when the server closes the connection (EOF).
pub fn frame_stream(
    mut client: SeedLinkClient,
) -> impl Stream<Item = Result<OwnedFrame, ClientError>> {
    async_stream::try_stream! {
        while let Some(frame) = client.next_frame().await? {
            yield frame;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockConfig, MockServer};
    use seedlink_rs_protocol::SequenceNumber;
    use seedlink_rs_protocol::frame::v3;
    use std::pin::pin;
    use tokio_stream::StreamExt;

    fn make_v3_frame(seq: u64, station: &str, network: &str) -> Vec<u8> {
        let mut payload = [0u8; v3::PAYLOAD_LEN];
        let sta_bytes = station.as_bytes();
        for (i, &b) in sta_bytes.iter().enumerate().take(5) {
            payload[8 + i] = b;
        }
        for i in sta_bytes.len()..5 {
            payload[8 + i] = b' ';
        }
        let net_bytes = network.as_bytes();
        for (i, &b) in net_bytes.iter().enumerate().take(2) {
            payload[18 + i] = b;
        }
        for i in net_bytes.len()..2 {
            payload[18 + i] = b' ';
        }
        v3::write(SequenceNumber::new(seq), &payload).unwrap()
    }

    #[tokio::test]
    async fn stream_yields_frames() {
        let frames = vec![
            make_v3_frame(1, "ANMO", "IU"),
            make_v3_frame(2, "ANMO", "IU"),
        ];
        let config = MockConfig {
            close_after_stream: true,
            ..MockConfig::v3_default(frames)
        };
        let server = MockServer::start(config).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let mut stream = pin!(frame_stream(client));

        let frame1 = stream.next().await.unwrap().unwrap();
        assert_eq!(frame1.sequence(), SequenceNumber::new(1));

        let frame2 = stream.next().await.unwrap().unwrap();
        assert_eq!(frame2.sequence(), SequenceNumber::new(2));

        // EOF → stream ends
        let end = stream.next().await;
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn stream_ends_on_eof() {
        let config = MockConfig {
            close_after_stream: true,
            ..MockConfig::v3_default(vec![])
        };
        let server = MockServer::start(config).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let mut stream = pin!(frame_stream(client));

        // No frames → immediate None
        let end = stream.next().await;
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn stream_collect_all() {
        let frames = vec![
            make_v3_frame(10, "ANMO", "IU"),
            make_v3_frame(11, "ANMO", "IU"),
            make_v3_frame(12, "ANMO", "IU"),
        ];
        let config = MockConfig {
            close_after_stream: true,
            ..MockConfig::v3_default(frames)
        };
        let server = MockServer::start(config).await;

        let mut client = SeedLinkClient::connect(&server.addr().to_string())
            .await
            .unwrap();

        client.station("ANMO", "IU").await.unwrap();
        client.data().await.unwrap();
        client.end_stream().await.unwrap();

        let stream = pin!(frame_stream(client));
        let collected: Vec<_> = stream.collect().await;
        assert_eq!(collected.len(), 3);
        assert_eq!(
            collected[0].as_ref().unwrap().sequence(),
            SequenceNumber::new(10)
        );
        assert_eq!(
            collected[2].as_ref().unwrap().sequence(),
            SequenceNumber::new(12)
        );
    }
}
