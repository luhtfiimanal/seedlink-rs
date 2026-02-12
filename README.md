# seedlink-rs

Pure Rust SeedLink client and server. Zero `unsafe`, zero C dependency.

Sister project of [miniseed-rs](https://github.com/luhtfiimanal/miniseed-rs).

## Crates

| Crate | Description |
|-------|-------------|
| [seedlink-rs-protocol](https://crates.io/crates/seedlink-rs-protocol) | SeedLink protocol types, commands, and frame parsing |
| [seedlink-rs-client](https://crates.io/crates/seedlink-rs-client) | Async SeedLink client (tokio) |
| [seedlink-rs-server](https://crates.io/crates/seedlink-rs-server) | Async SeedLink server (tokio) |

## Quick Start — Client

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

## Quick Start — Server

```rust
use seedlink_rs_server::SeedLinkServer;

#[tokio::main]
async fn main() -> seedlink_rs_server::Result<()> {
    let server = SeedLinkServer::bind("0.0.0.0:18000").await?;
    let store = server.store().clone();

    tokio::spawn(server.run());

    // Push miniSEED records from any source
    let payload = vec![0u8; 512];
    store.push("IU", "ANMO", &payload);
    Ok(())
}
```

## Features

### Protocol (`seedlink-rs-protocol`)

- Full SeedLink v3 and v4 command parsing and serialization (14 commands)
- v3 fixed frames (520 bytes) and v4 variable-length frames
- Sequence numbers: v3 hex (24-bit) and v4 decimal (64-bit)
- Response parsing with error codes
- INFO levels: ID, STATIONS, STREAMS, CONNECTIONS, and more
- Version-aware validation — prevents sending v3-only commands on v4

### Client (`seedlink-rs-client`)

- Async TCP client with state machine enforcement
- Automatic v4 protocol negotiation (falls back to v3)
- Station/channel selection with SELECT pattern filtering
- `TIME` command for time-windowed data requests
- `DATA` resume from last sequence number
- `FETCH` mode — stream buffered data then close
- `futures::Stream` impl via `into_stream()`
- Auto-reconnect with exponential backoff and per-station sequence resume
- Built-in deduplication — no duplicate frames after reconnect
- miniSEED decode via [miniseed-rs](https://github.com/luhtfiimanal/miniseed-rs)
- `tracing` integration for structured logging
- Configurable connect and read timeouts

### Server (`seedlink-rs-server`)

- Async TCP server — multiple concurrent clients
- In-memory ring buffer with configurable capacity
- Dual protocol: v3 and v4 frame streaming (auto-adapts per client)
- Multi-station subscription per client
- SELECT pattern filtering with `?` wildcards (`BHZ`, `BH?`, `00BHZ.D`)
- TIME filtering — parses miniSEED BTime, filters by time window
- INFO responses: ID, STATIONS, STREAMS, CONNECTIONS (XML)
- Connection tracking — protocol version, user agent, state
- USERAGENT and BATCH command support
- FETCH mode — send buffered data then close
- Graceful shutdown via `ShutdownHandle`

## Compatibility

Tested against real SeedLink servers:

- IRIS: `rtserve.iris.washington.edu:18000`
- GEOFON: `geofon.gfz-potsdam.de:18000`

219 tests passing across all three crates.

## Documentation

See [docs/FEATURES.md](docs/FEATURES.md) for comprehensive feature documentation.

## License

Apache-2.0
