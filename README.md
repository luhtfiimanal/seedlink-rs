# seedlink-rs

Pure Rust SeedLink client and server. Zero unsafe, zero C dependency.

Sister project of [miniseed-rs](https://github.com/luhtfiimanal/miniseed-rs).

## Crates

| Crate | Description |
|-------|-------------|
| [seedlink-rs-protocol](https://crates.io/crates/seedlink-rs-protocol) | SeedLink protocol types, commands, and frame parsing |
| [seedlink-rs-client](https://crates.io/crates/seedlink-rs-client) | Async SeedLink client (tokio) |
| seedlink-rs-server | Async SeedLink server (tokio) — coming soon |

## Quick Start

```rust
use seedlink_rs_client::SeedLinkClient;

#[tokio::main]
async fn main() -> seedlink_rs_client::Result<()> {
    let mut client = SeedLinkClient::connect("rtserve.iris.washington.edu:18000").await?;
    client.station("ANMO", "IU").await?;
    client.select("BHZ").await?;
    client.data().await?;
    client.end_stream().await?;

    while let Some(frame) = client.next_frame().await? {
        println!("seq={}, payload={} bytes", frame.sequence(), frame.payload().len());
    }
    Ok(())
}
```

## Status

**seedlink-rs-protocol** and **seedlink-rs-client** are published and functional.
Tested against IRIS (`rtserve.iris.washington.edu:18000`) with 139 tests passing.

### What works today

- SeedLink v3 and v4 protocol negotiation
- Station/channel selection, DATA, FETCH, END, INFO, BYE
- Sequence tracking and resume from last sequence
- Configurable connect and read timeouts
- Clean EOF handling (`next_frame()` returns `None`)

### Roadmap

- [ ] `TIME` command — time-windowed data requests (v3)
- [ ] `futures::Stream` impl for frame iteration
- [ ] Auto-reconnect with backoff
- [ ] `tracing` integration for structured logging
- [ ] `seedlink-rs-server` — async SeedLink server for data distribution
- [ ] miniSEED decode integration with [miniseed-rs](https://github.com/luhtfiimanal/miniseed-rs)

## License

Apache-2.0
