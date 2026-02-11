# seedlink-rs Architecture

## Overview

seedlink-rs is a pure Rust workspace for SeedLink real-time seismic data streaming.
Three crates: `seedlink-protocol` (shared types), `seedlink-client` (async client),
`seedlink-server` (async server).

Constraints: zero unsafe, zero C dependency, Apache 2.0.

## SeedLink Protocol

SeedLink is a TCP-based protocol for real-time seismic data streaming, developed
by GEOFON/GFZ Potsdam. Two versions exist:

### Version 3 (de facto standard)

No formal specification document exists. The protocol was defined through
implementations (SeisComP3, libslink, ringserver). Most public servers speak v3.

Frame format — fixed 520 bytes:

```
Offset  Size  Field
[0..2)   2    Signature: "SL" (ASCII)
[2..8)   6    Sequence number (6 uppercase hex digits, e.g., "00004F")
[8..520) 512  miniSEED v2 record (fixed size)
```

### Version 4 (FDSN adopted standard, Jan 2024)

Official specification: https://docs.fdsn.org/projects/seedlink/

Frame format — variable length:

```
Offset    Size     Field
[0..2)     2       Signature: "SE" (ASCII)
[2..3)     1       Payload format ('2'=miniSEED2, '3'=miniSEED3, 'J'=JSON, 'X'=XML)
[3..4)     1       Payload subformat ('D'=data, 'E'=event, 'I'=info, etc.)
[4..8)     4       Payload length (UINT32, little-endian)
[8..16)    8       Sequence number (UINT64, little-endian)
[16..17)   1       Station ID length (UINT8)
[17..17+N) N       Station ID (ASCII, variable)
[17+N..)   var     Payload (binary)
```

### Version Negotiation

A server can support both v3 and v4 on the same port. Negotiation:

1. Client sends `HELLO`
2. Server responds with capabilities: `SeedLink v4.0 ... :: SLPROTO:4.0 SLPROTO:3.1`
3. Client sends `SLPROTO 4.0` to upgrade, or skips it to stay on v3
4. All subsequent frames/commands follow the negotiated version

### Key Differences

| Aspect               | v3                          | v4                              |
|-----------------------|-----------------------------|---------------------------------|
| Frame signature       | `"SL"` (2 bytes)           | `"SE"` (2 bytes)               |
| Frame size            | Fixed 520 bytes             | Variable length                 |
| Sequence in frame     | 6 hex ASCII chars           | 8-byte u64 LE binary            |
| Sequence in commands  | Hex (`DATA 00001A`)         | Decimal (`DATA 26`)             |
| Sequence range        | 0-0xFFFFFF (~16.7M)        | Full u64                        |
| Payload               | miniSEED v2 only            | miniSEED v2/v3, JSON, XML       |
| Station in commands   | `STATION sta net` (2 args)  | `STATION NET_STA` (combined)    |
| Select pattern        | `??.BHZ`                    | `*_B_H_Z` (FDSN style)         |
| Time format           | `2024,1,15,0,0,0` (comma)  | ISO 8601                        |
| INFO response         | XML in miniSEED log records | JSON in v4 frames               |
| Error response        | `ERROR\r` (no code)         | `ERROR code description\r\n`    |
| Auth                  | None                        | `AUTH` command (USERPASS, JWT)   |

## Sequence Numbers

SeedLink sequence numbers are **server-assigned, global counters** that index
packets in the server's ring buffer. They are NOT miniSEED sequence numbers.

```
Ring Buffer (server):
  [seq 000001] IU.ANMO.00.BHZ miniSEED record
  [seq 000002] GE.WLF.00.BHE  miniSEED record    <- different station, seq increments
  [seq 000003] IU.ANMO.00.BHZ miniSEED record
  ...
```

Key properties:

- **Per-server** — one global counter, shared across all stations/channels
- **Assigned by server** — not by the data producer or client
- **Purpose** — enable client resume after disconnect
- **Independent from miniSEED seq** — no mapping needed between them

### Resume Flow

Client saves last received sequence per station. On reconnect:

```
Client                          Server
  |--- STATION ANMO IU -------->|
  |--- SELECT ??.BHZ ---------->|
  |--- DATA 00004F ------------>|   <- resume from here for ANMO
  |--- STATION WLF GE --------->|
  |--- SELECT ??.BH? ---------->|
  |--- DATA 000123 ------------>|   <- resume from here for WLF
  |--- END --------------------->|
  |<-- frames from 000050... ---|   <- server sends from seq+1 onward
```

### Who Remembers What

| What                       | Who            | How                          |
|----------------------------|----------------|------------------------------|
| Last received seq/station  | **Client**     | State file or in-memory map  |
| Recent packets + sequences | **Server**     | Ring buffer (FIFO)           |
| seq miniSEED <-> seq SeedLink | **Nobody** | They are independent         |

If a client requests a sequence that has fallen off the ring buffer, the server
starts from the oldest available packet.

## Workspace Architecture

### `seedlink-protocol`

Pure synchronous crate. No async, no tokio, no Arc. Handles:

- Protocol types: commands, responses, frames, sequence numbers
- Parsing: bytes -> structured types (zero-copy where possible)
- Serialization: structured types -> bytes
- Both v3 and v4 support

Design:

- `Command` enum — unified for v3+v4, serialization takes `ProtocolVersion` param
- `RawFrame<'a>` — zero-copy frame, borrows payload from input buffer
- `DataFrame` — owned frame with decoded `MseedRecord`
- `SequenceNumber(u64)` — internal u64, format methods for v3 hex / v4 decimal

### `seedlink-client`

Async TCP client (tokio). No Arc. Uses `&mut self` pattern:

```rust
let mut client = SeedLinkClient::connect("rtserve.iris.washington.edu:18000").await?;
client.hello().await?;
client.station("ANMO", "IU").await?;
client.select("??.BHZ").await?;
while let Some(frame) = client.next_frame().await? {
    // process frame
}
```

Responsibilities:
- TCP connection management
- Protocol handshake (version negotiation)
- Streaming frames as `async Stream`
- Reconnection with backoff
- Per-station sequence tracking (state file for resume)

### `seedlink-server`

Async TCP server (tokio). Arc only for shared state with clear ownership:

```rust
let server = SeedLinkServer::bind("0.0.0.0:18000").await?;
server.publish(mseed_record).await;  // server assigns seq, stores in ring buffer
```

Responsibilities:
- TCP listener, per-client tasks
- Command parsing from clients
- Ring buffer (internal, default implementation)
- Sequence number assignment
- Client subscription and data distribution
- v3/v4 dual protocol support

Arc policy:
- `Arc<SharedState>` cloned into each connection task
- Arc lifetime = connection task lifetime (auto-drop on disconnect)
- NO `HashMap<ClientId, Arc<...>>` pattern (use channels instead)
- Channels self-clean: `Sender::send()` returns `Err` when receiver drops

## Oracle / Reference Implementations

| Reference    | Language | Role                                    |
|-------------|----------|-----------------------------------------|
| libslink     | C        | Canonical client library (EarthScope)   |
| ringserver   | C        | Reference server (EarthScope)           |
| slinktool    | C        | CLI client for testing                  |
| ObsPy        | Python   | TDD oracle in pyscripts/                |
| FDSN spec    | Docs     | Official v4 specification               |

Public test servers:
- `rtserve.iris.washington.edu:18000` (IRIS/EarthScope)
- `geofon.gfz-potsdam.de:18000` (GEOFON/GFZ)

## Dependencies

- `miniseed-rs` 0.2 — pure Rust miniSEED v2/v3 codec (sister project)
- `tokio` — async runtime (client + server only)
- `thiserror` — error type derive macros
