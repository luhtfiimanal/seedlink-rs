# seedlink-rs — Feature Documentation

Pure Rust SeedLink client and server. Zero `unsafe`, zero C dependency. Apache-2.0.

All three crates are at **v0.2.0** with **219 tests** passing.

---

## Table of Contents

- [Workspace Overview](#workspace-overview)
- [seedlink-rs-protocol](#seedlink-rs-protocol)
  - [Commands](#commands)
  - [Responses](#responses)
  - [Frame Formats](#frame-formats)
  - [Sequence Numbers](#sequence-numbers)
  - [INFO Levels](#info-levels)
  - [Protocol Versions](#protocol-versions)
  - [Error Types (Protocol)](#error-types-protocol)
- [seedlink-rs-client](#seedlink-rs-client)
  - [SeedLinkClient](#seedlinkclient)
  - [Client State Machine](#client-state-machine)
  - [Configuration](#client-configuration)
  - [Streaming & Frames](#streaming--frames)
  - [ReconnectingClient](#reconnectingclient)
  - [Error Types (Client)](#error-types-client)
- [seedlink-rs-server](#seedlink-rs-server)
  - [SeedLinkServer](#seedlinkserver)
  - [Server Configuration](#server-configuration)
  - [DataStore & Ring Buffer](#datastore--ring-buffer)
  - [Subscription Filtering](#subscription-filtering)
  - [SELECT Pattern Matching](#select-pattern-matching)
  - [TIME Filtering](#time-filtering)
  - [INFO Responses](#info-responses)
  - [Connection Tracking](#connection-tracking)
  - [Command Handling](#command-handling)
  - [Graceful Shutdown](#graceful-shutdown)
  - [Error Types (Server)](#error-types-server)
- [Protocol Reference](#protocol-reference)
  - [Connection Flow](#connection-flow)
  - [Frame Format (v3)](#frame-format-v3)
  - [Frame Format (v4)](#frame-format-v4)
  - [miniSEED v2 Header Offsets](#miniseed-v2-header-offsets)
- [Test Coverage](#test-coverage)

---

## Workspace Overview

```
seedlink-rs/                  # Cargo workspace
  seedlink-protocol/          # Crate: seedlink-rs-protocol
  seedlink-client/            # Crate: seedlink-rs-client
  seedlink-server/            # Crate: seedlink-rs-server
  pyscripts/                  # TDD oracle (Python, uv + ruff + basedpyright)
  docs/                       # Documentation
```

| Crate | crates.io | Description |
|-------|-----------|-------------|
| `seedlink-protocol/` | `seedlink-rs-protocol` | Shared protocol types, commands, frame parsing |
| `seedlink-client/` | `seedlink-rs-client` | Async SeedLink client (tokio) |
| `seedlink-server/` | `seedlink-rs-server` | Async SeedLink server (tokio) |

---

## seedlink-rs-protocol

Shared protocol layer for SeedLink v3/v4, used by both client and server.

### Commands

All 14 SeedLink commands are implemented with version-aware parsing and serialization.

#### Both v3 and v4

| Command | Format | Description |
|---------|--------|-------------|
| `HELLO` | `HELLO` | Handshake — server returns software info + organization |
| `STATION` | `STATION sta net` (v3) / `STATION net_sta` (v4) | Select station and network |
| `SELECT` | `SELECT pattern` | Select channels (e.g., `BHZ`, `BH?`, `00BHZ.D`) |
| `DATA` | `DATA [seq] [start] [end]` | Arm subscription, optionally resume from sequence |
| `END` | `END` | Trigger continuous binary streaming |
| `BYE` | `BYE` | Close connection |
| `INFO` | `INFO level` | Request server information |

#### v3 Only

| Command | Format | Description |
|---------|--------|-------------|
| `BATCH` | `BATCH` | Batch mode for multiple stations |
| `FETCH` | `FETCH [seq]` | Stream buffered data then close connection |
| `TIME` | `TIME start [end]` | Request data within a time window |
| `CAT` | `CAT` | Station catalog listing |

#### v4 Only

| Command | Format | Description |
|---------|--------|-------------|
| `SLPROTO` | `SLPROTO version` | Negotiate protocol version (e.g., `4.0`) |
| `AUTH` | `AUTH value` | Authentication |
| `USERAGENT` | `USERAGENT description` | Client identification string |
| `ENDFETCH` | `ENDFETCH` | End fetch operation |

**Features:**
- Case-insensitive parsing (`hello` = `HELLO`)
- Version validation: `Command::is_valid_for(ProtocolVersion)` — prevents sending v3-only commands on v4 and vice versa
- Roundtrip: `parse()` → `to_bytes()` → `parse()` produces identical results
- Sequence number format adapts to version: 6-digit hex (v3) vs decimal (v4)

### Responses

| Response | Wire Format | Description |
|----------|-------------|-------------|
| `OK` | `OK\r\n` | Success |
| `ERROR` | `ERROR [code] [description]\r\n` | Error with optional code |
| `END` | `END\r\n` | Stream/info termination |
| `HELLO` | Two lines: software + organization | Server identification |

**Error codes:** `UNSUPPORTED`, `UNEXPECTED`, `UNAUTHORIZED`, `LIMIT`, `ARGUMENTS`, `AUTH`, `INTERNAL`

### Frame Formats

#### v3 Frames

Fixed 520 bytes:

```
[0..2]    "SL" signature
[2..8]    Sequence number (6 hex digits, e.g., "00001A")
[8..520]  miniSEED v2 record payload (512 bytes)
```

#### v4 Frames

Variable-length:

```
[0..2]      "SE" signature
[2]         Format byte (MiniSeed2, MiniSeed3, Json, Xml)
[3]         Subformat byte (Data, Event, Calibration, Timing, Log, Opaque, Info, InfoError)
[4..8]      Payload length (u32 LE)
[8..16]     Sequence number (u64 LE)
[16]        Station ID length
[17..17+N]  Station ID (UTF-8 string)
[17+N..]    Payload (variable length)
```

**Payload formats:** `MiniSeed2` (0x01), `MiniSeed3` (0x02), `Json` (0x03), `Xml` (0x04)

**Payload subformats:** `Data` (0x01), `Event` (0x02), `Calibration` (0x03), `Timing` (0x04), `Log` (0x05), `Opaque` (0x06), `Info` (0x07), `InfoError` (0x08)

### Sequence Numbers

| Property | v3 | v4 |
|----------|----|----|
| Wire format | 6 hex digits (`"00001A"`) | Decimal string (`"26"`) |
| Binary format | N/A | 8 bytes LE |
| Maximum | `0xFFFFFF` (16,777,215) | `u64::MAX - 2` |
| Wrapping | Wraps at `V3_MAX` back to 1 | No practical wrapping |

**Special values:**
- `UNSET` (`u64::MAX`) — sequence not yet assigned
- `ALL_DATA` (`u64::MAX - 1`) — request all data (v4)

### INFO Levels

| Level | v3 | v4 | Description |
|-------|----|----|-------------|
| `ID` | Yes | Yes | Server identification |
| `STATIONS` | Yes | Yes | Station list with sequence ranges |
| `STREAMS` | Yes | Yes | Stream detail (channel, location, type) |
| `CONNECTIONS` | Yes | Yes | Active client connections |
| `GAPS` | Yes | No | Gap information |
| `ALL` | Yes | No | All information |
| `FORMATS` | No | Yes | Supported payload formats |
| `CAPABILITIES` | No | Yes | Server capabilities |

### Protocol Versions

- `ProtocolVersion::V3` — SeedLink v3.x (default)
- `ProtocolVersion::V4` — SeedLink v4.x (negotiated via `SLPROTO 4.0`)

### Error Types (Protocol)

| Error | Description |
|-------|-------------|
| `FrameTooShort` | Frame shorter than minimum size |
| `InvalidSignature` | Frame signature not `"SL"` (v3) or `"SE"` (v4) |
| `InvalidSequence` | Sequence number parsing failure |
| `InvalidCommand` | Command parsing error |
| `VersionMismatch` | Command not valid for negotiated protocol version |
| `InvalidResponse` | Response parsing error |
| `InvalidInfoLevel` | Unknown INFO level string |
| `InvalidPayloadFormat` | Unknown v4 payload format byte |
| `InvalidPayloadSubformat` | Unknown v4 payload subformat byte |
| `PayloadLengthMismatch` | Payload size doesn't match header |
| `Miniseed` | miniSEED decoding error |

---

## seedlink-rs-client

Async SeedLink client built on tokio. Connects to any SeedLink v3/v4 server (IRIS, BMKG, GEOFON, etc.).

### SeedLinkClient

The primary client type. Manages a single TCP connection with state machine enforcement.

```rust
use seedlink_rs_client::SeedLinkClient;

let mut client = SeedLinkClient::connect("rtserve.iris.washington.edu:18000").await?;

// Automatic v4 negotiation if server supports it
println!("Protocol: {:?}", client.version());        // V3 or V4
println!("Server: {}", client.server_info().software); // "SeedLink v3.1 ..."

// Configure subscriptions
client.station("ANMO", "IU").await?;
client.select("BHZ").await?;         // Optional: filter by channel
client.data().await?;                 // Arm subscription

// Multi-station: repeat STATION/SELECT/DATA blocks
client.station("WLF", "GE").await?;
client.data().await?;

// Start streaming
client.end_stream().await?;

// Read frames
while let Some(frame) = client.next_frame().await? {
    println!("seq={}, {} bytes", frame.sequence(), frame.payload().len());
}

client.bye().await?;
```

**Methods:**

| Method | State Requirement | Description |
|--------|-------------------|-------------|
| `connect(addr)` | — | Connect with default config |
| `connect_with_config(addr, config)` | — | Connect with custom config |
| `station(sta, net)` | Connected/Configured | Select station |
| `select(pattern)` | Configured | Filter channels |
| `data()` | Configured | Arm from beginning |
| `data_from(seq)` | Configured | Resume from sequence |
| `time_window(start, end?)` | Configured | Time range filter (v3 only) |
| `end_stream()` | Configured | Start continuous streaming |
| `fetch()` | Configured | Stream buffered then close (v3 only) |
| `fetch_from(seq)` | Configured | Resume fetch (v3 only) |
| `next_frame()` | Streaming | Read next frame (`None` = EOF) |
| `into_stream()` | Streaming | Convert to `futures::Stream` |
| `info(level)` | Connected/Configured | Request INFO response |
| `bye()` | Any | Close connection |
| `version()` | Any | Negotiated protocol version |
| `server_info()` | Any | Server metadata from HELLO |
| `state()` | Any | Current state |
| `last_sequence(net, sta)` | Any | Last received sequence per station |
| `sequences()` | Any | All tracked sequence numbers |

### Client State Machine

```
Disconnected → connect() → Connected
Connected → station() → Configured
Configured → station()/select()/data()/time_window() → Configured
Configured → end_stream()/fetch() → Streaming
Streaming → next_frame() returns None → Disconnected
Any → bye() → Disconnected
```

Methods enforce valid state transitions at runtime. Calling a method in the wrong state returns `ClientError::InvalidState`.

### Client Configuration

```rust
use seedlink_rs_client::ClientConfig;
use std::time::Duration;

let config = ClientConfig {
    connect_timeout: Duration::from_secs(10),  // TCP connect timeout (default: 10s)
    read_timeout: Duration::from_secs(30),     // Per-read timeout (default: 30s)
    prefer_v4: true,                           // Auto-negotiate v4 (default: true)
};
let client = SeedLinkClient::connect_with_config("server:18000", config).await?;
```

### Streaming & Frames

Frames are returned as `OwnedFrame` with two variants:

```rust
match frame {
    OwnedFrame::V3 { sequence, payload } => {
        // payload: 512-byte miniSEED v2 record
    }
    OwnedFrame::V4 { format, subformat, sequence, station_id, payload } => {
        // station_id: "IU_ANMO"
        // payload: variable-length
    }
}

// Common methods on OwnedFrame:
frame.sequence()                // SequenceNumber
frame.payload()                 // &[u8]
frame.station_key()             // Option<StationKey> (extracted from v4 station_id)
frame.decode()                  // Parse miniSEED via miniseed-rs
```

**Stream trait:**

```rust
use futures::StreamExt;

let mut stream = client.into_stream();
while let Some(result) = stream.next().await {
    let frame = result?;
    // ...
}
```

### ReconnectingClient

Auto-reconnecting wrapper that replays subscriptions and deduplicates frames.

```rust
use seedlink_rs_client::ReconnectingClient;

let mut client = ReconnectingClient::connect("server:18000").await?;
client.station("ANMO", "IU").await?;
client.data().await?;
client.end_stream().await?;

// If connection drops, automatically:
// 1. Reconnects with exponential backoff
// 2. Replays STATION/SELECT/DATA with resume sequences
// 3. Deduplicates frames (skips seq <= last received)
while let Some(frame) = client.next_frame().await? {
    // No duplicates, guaranteed
}
```

**Configuration:**

```rust
use seedlink_rs_client::ReconnectConfig;

let config = ReconnectConfig {
    initial_backoff: Duration::from_secs(1),   // First retry delay (default: 1s)
    max_backoff: Duration::from_secs(60),      // Maximum delay (default: 60s)
    multiplier: 2.0,                           // Backoff multiplier (default: 2.0)
    max_attempts: 0,                           // 0 = unlimited retries (default: 0)
};
```

**Reconnect behavior:**
- Records all subscription steps (STATION, SELECT, DATA, TIME)
- On reconnect, replays steps with `DATA seq` using last known sequence per station
- Frames with `seq <= last_tracked` are silently dropped (deduplication)
- Supports `into_stream()` for async Stream with auto-reconnect

### Error Types (Client)

| Error | Description |
|-------|-------------|
| `Io` | TCP/socket I/O error |
| `Protocol` | SeedLink protocol parsing error |
| `Timeout` | Operation exceeded configured timeout |
| `Disconnected` | Server closed connection |
| `ServerError` | Server returned ERROR response |
| `InvalidState` | Method called in wrong state |
| `NegotiationFailed` | v4 protocol negotiation failed |
| `UnexpectedResponse` | Unexpected server response |
| `ReconnectFailed` | Auto-reconnect exhausted all attempts |

---

## seedlink-rs-server

Async SeedLink server for real-time seismic data distribution. Accepts client connections and distributes miniSEED records from an in-memory ring buffer.

### SeedLinkServer

```rust
use seedlink_rs_server::{SeedLinkServer, ServerConfig};

let server = SeedLinkServer::bind("0.0.0.0:18000").await?;
let store = server.store().clone();

tokio::spawn(server.run());

// Push data from any source (e.g., data acquisition, file reader, upstream client)
let payload = vec![0u8; 512]; // miniSEED v2 record
store.push("IU", "ANMO", &payload);
```

### Server Configuration

```rust
let config = ServerConfig {
    software: "SeedLink".to_owned(),       // HELLO software name (default: "SeedLink")
    version: "v3.1".to_owned(),            // HELLO version (default: "v3.1")
    organization: "seedlink-rs".to_owned(), // HELLO organization (default: "seedlink-rs")
    ring_capacity: 10_000,                 // Ring buffer size (default: 10,000 records)
};
let server = SeedLinkServer::bind_with_config("0.0.0.0:18000", config).await?;
```

### DataStore & Ring Buffer

Thread-safe (`Clone` = cheap Arc) data store backed by a circular ring buffer.

```rust
let store = server.store().clone();

// Push records (must be exactly 512 bytes)
let seq = store.push("IU", "ANMO", &payload);

// Ring buffer evicts oldest records when capacity is exceeded
// Sequence numbers are monotonically increasing (wrap at V3_MAX → 1)
```

**Internal behavior:**
- `push()` assigns a monotonic sequence number and notifies waiting clients
- `read_since(cursor, subscriptions)` returns matching records after cursor
- Subscription filtering: network + station + SELECT patterns + TIME window
- `station_info()` / `stream_info()` enumerate unique stations/streams in the ring

### Subscription Filtering

Each client subscription specifies:
1. **Network + Station** — exact match (case-insensitive)
2. **SELECT patterns** — channel/location/type filtering (OR logic: any pattern match passes)
3. **TIME window** — timestamp range filtering (extracted from miniSEED BTime)

Records pass only if ALL three criteria match. Multiple `STATION` blocks create independent subscriptions merged into a single output stream.

### SELECT Pattern Matching

Pattern format: `[LL]CCC[.T]`

| Component | Bytes | Description |
|-----------|-------|-------------|
| `LL` | 2 chars (optional) | Location code (miniSEED bytes 13-14) |
| `CCC` | 3 chars (required) | Channel code (miniSEED bytes 15-17) |
| `.T` | suffix (optional) | Data type/quality indicator (miniSEED byte 6) |

**Wildcard:** `?` matches any single character.

**Examples:**

| Pattern | Matches | Does NOT Match |
|---------|---------|----------------|
| `BHZ` | `00.BHZ`, `10.BHZ` (any location) | `BHN`, `LHZ` |
| `BH?` | `BHZ`, `BHN`, `BHE` | `LHZ`, `SHZ` |
| `00BHZ` | `00.BHZ` | `10.BHZ` |
| `??BHZ` | `00.BHZ`, `10.BHZ`, any location | `BHN` |
| `BHZ.D` | `BHZ` with quality `D` | `BHZ` with quality `R` |
| `00BHZ.D` | `00.BHZ` with quality `D` | `10.BHZ`, quality `R` |
| `Z` | Any channel ending in Z (`BHZ`, `LHZ`, `SHZ`) | `BHN`, `BHE` |

**No SELECT = match all channels** (pass-through).

### TIME Filtering

The `TIME` command attaches a time window to the current station subscription.

```
TIME 2024,1,1,0,0,0 2024,1,31,23,59,59    # Jan 1 to Jan 31, 2024
TIME 2024,6,1,0,0,0                         # Jun 1, 2024 onwards (open-ended)
```

**Format:** `YYYY,M,D,h,m,s` (year, month, day, hour, minute, second)

**How it works:**
1. Parses the TIME command format (`month/day` based) into a `Timestamp` (seconds since epoch)
2. For each record in the ring, parses the miniSEED BTime (`day-of-year` based, bytes 20-30) into a `Timestamp`
3. Checks if the record's timestamp falls within `[start, end]`
4. Records with unparseable BTime are rejected

**Leap year aware:** Correctly handles Feb 29 and 366-day years.

**Open-ended:** If no `end` time is specified, all records after `start` pass.

### INFO Responses

The server generates XML responses for INFO requests:

#### INFO ID

```xml
<?xml version="1.0"?>
<seedlink software="SeedLink v3.1" organization="seedlink-rs" started="2026/02/12 10:30:00"/>
```

#### INFO STATIONS

```xml
<?xml version="1.0"?>
<seedlink>
  <station name="ANMO" network="IU" description="" begin_seq="000001" end_seq="000005" stream_check="enabled"/>
  <station name="WLF" network="GE" description="" begin_seq="000002" end_seq="000003" stream_check="enabled"/>
</seedlink>
```

#### INFO STREAMS

```xml
<?xml version="1.0"?>
<seedlink>
  <station name="ANMO" network="IU">
    <stream seedname="BHZ" location="00" type="D" begin_seq="000001" end_seq="000003"/>
    <stream seedname="BHN" location="00" type="D" begin_seq="000002" end_seq="000004"/>
  </station>
</seedlink>
```

#### INFO CONNECTIONS

```xml
<?xml version="1.0"?>
<seedlink>
  <connection host="127.0.0.1:54321" port="54321" ctime="2026/02/12 10:30:00" proto="3.1" useragent="seedlink-rs/0.2" state="Streaming"/>
  <connection host="127.0.0.1:54322" port="54322" ctime="2026/02/12 10:31:00" proto="4.0" useragent="" state="Connected"/>
</seedlink>
```

**Supported levels:** `ID`, `STATIONS`, `STREAMS`, `CONNECTIONS`

**Unsupported levels** (`GAPS`, `ALL`, `FORMATS`, `CAPABILITIES`) return `ERROR UNSUPPORTED`.

### Connection Tracking

Every client connection is registered in a thread-safe `ConnectionRegistry`:

- **On connect:** Assigned a unique monotonic ID, recorded with address and timestamp
- **State updates:** Tracked as `Connected` → `Configured` → `Streaming`
- **Protocol negotiation:** `SLPROTO 4.0` updates the registry entry
- **USERAGENT:** Stores the client identifier string
- **On disconnect:** Automatically unregistered (BYE, EOF, or shutdown)
- **INFO CONNECTIONS:** Snapshots the registry and generates XML listing all active clients

### Command Handling

The server processes all commonly-used SeedLink v3/v4 commands:

| Command | Server Behavior |
|---------|-----------------|
| `HELLO` | Returns 2-line response: software info + organization. Advertises `SLPROTO:4.0 SLPROTO:3.1` |
| `SLPROTO 4.0` | Negotiates v4 protocol. Returns `OK`. Subsequent frames use v4 format |
| `STATION sta net` | Creates a new subscription. Returns `OK` |
| `SELECT pattern` | Parses pattern, attaches to last subscription. Returns `OK` or `ERROR` |
| `DATA [seq]` | Sets resume cursor. Returns `OK` |
| `TIME start [end]` | Parses time window, attaches to last subscription. Returns `OK` or `ERROR` |
| `FETCH [seq]` | Streams buffered records matching subscriptions, then closes connection |
| `END` | Starts continuous streaming. Waits for new data indefinitely |
| `INFO level` | Generates XML, sends as frame(s) + `END` |
| `USERAGENT desc` | Stores client identifier. Returns `OK` |
| `BATCH` | Acknowledged. Returns `OK` |
| `BYE` | Closes connection |
| Unknown | Returns `ERROR UNSUPPORTED` |

**Streaming modes:**
- **Continuous (END):** Sends all matching records, then waits for new data. Loops forever until client disconnects or server shuts down
- **One-shot (FETCH):** Sends all matching buffered records, then closes the connection

**Frame format:** Automatically adapts to the negotiated protocol version:
- v3: Fixed 520-byte frames (`SL` + 6-hex-digit seq + 512-byte payload)
- v4: Variable-length frames (`SE` + format/subformat + seq + station_id + payload)

### Graceful Shutdown

```rust
let server = SeedLinkServer::bind("0.0.0.0:18000").await?;
let handle = server.shutdown_handle();

tokio::spawn(server.run());

// Later...
handle.shutdown();
// - Accept loop stops
// - All streaming clients get connection close
// - New connections are rejected
```

### Error Types (Server)

| Error | Description |
|-------|-------------|
| `Io` | General I/O error |
| `Protocol` | SeedLink protocol error |
| `Bind` | Failed to bind TCP listener |
| `InvalidPayloadLength` | Payload not exactly 512 bytes |

---

## Protocol Reference

### Connection Flow

```
Client                              Server
  |                                   |
  |--- HELLO ----------------------->|
  |<-- "SeedLink v3.1 ..." ---------|   (line 1: software + capabilities)
  |<-- "seedlink-rs" ---------------|   (line 2: organization)
  |                                   |
  |--- SLPROTO 4.0 ---------------->|   (optional: v4 negotiation)
  |<-- OK --------------------------|
  |                                   |
  |--- USERAGENT my-app/1.0 ------->|   (optional: client identification)
  |<-- OK --------------------------|
  |                                   |
  |--- STATION ANMO IU ------------->|
  |<-- OK --------------------------|
  |                                   |
  |--- SELECT BHZ ------------------>|   (optional: channel filter)
  |<-- OK --------------------------|
  |                                   |
  |--- TIME 2024,1,1,0,0,0 -------->|   (optional: time filter)
  |<-- OK --------------------------|
  |                                   |
  |--- DATA ------------------------>|
  |<-- OK --------------------------|
  |                                   |
  |--- STATION WLF GE -------------->|   (optional: multi-station)
  |<-- OK --------------------------|
  |--- DATA ------------------------>|
  |<-- OK --------------------------|
  |                                   |
  |--- END ------------------------->|   (triggers streaming)
  |                                   |
  |<-- [SL frame: seq=1] -----------|   (binary streaming starts)
  |<-- [SL frame: seq=2] -----------|
  |<-- [SL frame: seq=3] -----------|
  |    ...                            |
  |                                   |
  |--- BYE ------------------------->|   (close connection)
```

### Frame Format (v3)

```
Offset  Size  Description
──────  ────  ─────────────────────────────
0       2     Signature: "SL" (0x53, 0x4C)
2       6     Sequence: 6 hex ASCII digits
8       512   miniSEED v2 record payload
──────────────────────────────────────────
Total: 520 bytes (fixed)
```

### Frame Format (v4)

```
Offset        Size      Description
──────        ────      ─────────────────────────
0             2         Signature: "SE" (0x53, 0x45)
2             1         Payload format byte
3             1         Payload subformat byte
4             4         Payload length (u32 LE)
8             8         Sequence number (u64 LE)
16            1         Station ID length (N)
17            N         Station ID (UTF-8)
17+N          variable  Payload
───────────────────────────────────────────────────
Total: 17 + N + payload_length bytes (variable)
```

### miniSEED v2 Header Offsets

Key offsets used by the server for station/channel extraction and time filtering:

```
Offset  Size  Field
──────  ────  ────────────────────────
0       6     Sequence number (ASCII)
6       1     Quality/type indicator ('D', 'R', 'Q', 'M')
7       1     Reserved
8       5     Station code (space-padded)
13      2     Location code (space-padded)
15      3     Channel code (space-padded)
18      2     Network code (space-padded)
20      2     BTime: year (u16 BE)
22      2     BTime: day-of-year (u16 BE)
24      1     BTime: hour
25      1     BTime: minute
26      1     BTime: second
27      1     BTime: unused
28      2     BTime: 0.0001s ticks (u16 BE)
30      2     Number of samples
32      2     Sample rate factor
34      2     Sample rate multiplier
```

---

## Test Coverage

**219 tests** across the workspace, all passing.

| Crate | Unit Tests | Integration Tests | Total |
|-------|------------|-------------------|-------|
| `seedlink-rs-protocol` | 93 | 5 (test vectors) | 98 |
| `seedlink-rs-client` | 47 | 9 (integration + test vectors) | 56 |
| `seedlink-rs-server` | 62 | — | 62 |
| Doc-tests | — | — | 3 |
| **Total** | | | **219** |

### Server Test Summary (28 tests)

| # | Test | Feature |
|---|------|---------|
| 1 | `hello_response` | HELLO + v4 negotiation |
| 2 | `station_data_end_receives_frames` | Basic STATION → DATA → END flow |
| 3 | `live_push_during_streaming` | Real-time push while client streams |
| 4 | `data_resume_from_sequence` | DATA with sequence resume |
| 5 | `multi_station_subscription` | Multiple STATION blocks |
| 6 | `station_filtering` | Network/station filtering |
| 7 | `bye_disconnects` | BYE command |
| 8 | `multiple_concurrent_clients` | Concurrent client connections |
| 9 | `ring_buffer_eviction` | Ring buffer capacity and eviction |
| 10 | `unknown_command_error` | Unknown command → ERROR UNSUPPORTED |
| 11 | `slproto_v4_negotiate_and_stream` | SLPROTO 4.0 → v4 frames |
| 12 | `v3_when_client_does_not_prefer_v4` | Force v3 frames |
| 13 | `fetch_sends_buffered_then_closes` | FETCH mode |
| 14 | `fetch_with_resume_sequence` | FETCH with sequence resume |
| 15 | `graceful_shutdown` | ShutdownHandle |
| 16 | `info_id_returns_xml` | INFO ID XML generation |
| 17 | `info_stations_returns_pushed_stations` | INFO STATIONS |
| 18 | `info_streams_returns_channel_detail` | INFO STREAMS |
| 19 | `info_unsupported_level_returns_error` | Unsupported INFO level |
| 20 | `select_filters_by_channel` | SELECT exact channel |
| 21 | `select_wildcard_pattern` | SELECT with `?` wildcards |
| 22 | `no_select_matches_all_channels` | No SELECT = all channels |
| 23 | `time_filtering_excludes_out_of_range` | TIME window rejects out-of-range |
| 24 | `time_filtering_open_ended` | TIME with no end date |
| 25 | `info_connections_lists_active_clients` | INFO CONNECTIONS lists clients |
| 26 | `useragent_accepted` | USERAGENT returns OK |
| 27 | `batch_mode_multiple_stations` | BATCH + multi-station |
| 28 | `connection_unregistered_on_disconnect` | Connection cleanup on BYE |

### Verification Commands

```bash
cargo build --workspace                    # Build all crates
cargo test --workspace                     # Run all 219 tests
cargo clippy --workspace -- -D warnings    # Lint (zero warnings)
cargo fmt --all -- --check                 # Format check
```

---

## Compatibility

**Tested against real servers:**
- IRIS: `rtserve.iris.washington.edu:18000`
- GEOFON: `geofon.gfz-potsdam.de:18000`

**Protocol support:**
- SeedLink v3.1 (full)
- SeedLink v4.0 (negotiation + frame format)

**Not yet implemented:**
- `AUTH` — authentication (v4)
- `CAT` — catalog listing (v3)
- INFO `GAPS` / `ALL` — extended info (v3)
- INFO `FORMATS` / `CAPABILITIES` — extended info (v4)
- Fire-and-forget mode (classic v3 without EXTREPLY)
