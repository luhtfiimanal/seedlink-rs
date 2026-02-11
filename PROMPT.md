# seedlink-rs — Agent Bootstrap Prompt

## What is this?

`seedlink-rs` is a pure Rust workspace for SeedLink real-time seismic data streaming. It provides a protocol parser, async client, and async server — all built on tokio and using `miniseed-rs` for miniSEED record handling.

**Goal**: Publishable to crates.io as three crates. Zero unsafe, zero C dependency. Apache 2.0 licensed.

Read `CLAUDE.md` for full workspace instructions, protocol details, and code quality rules.

## Implementation Roadmap

### Phase 1: Protocol Foundation (`seedlink-protocol`)
1. Define SeedLink command types: `Hello`, `Station`, `Select`, `Data`, `Time`, `End`, `Bye`, `Info`
2. Implement command serialization: `Command -> String` (client sends)
3. Implement response parsing: server responses, info XML
4. Define SeedLink frame: 8-byte header ("SL" + 6-char hex sequence) + 512-byte miniSEED
5. Frame parser: `&[u8]` → `SlFrame { sequence: u64, record: MseedRecord }`
6. TDD: capture real traffic from IRIS/GEOFON as test vectors

### Phase 2: Client (`seedlink-client`)
1. TCP connection with tokio `TcpStream`
2. Handshake: `HELLO` → parse server version
3. Station/channel selection: `STATION` + `SELECT`
4. Streaming: `DATA`/`END` → async `Stream<Item = Result<SlFrame>>`
5. Reconnection logic with configurable backoff
6. Multi-station support
7. TDD: mock server for unit tests, real IRIS server for integration tests

### Phase 3: Server (`seedlink-server`)
1. TCP listener with tokio
2. Accept connections, parse client commands
3. Station/channel management
4. Ring buffer for recent data
5. Client subscription and data distribution
6. INFO response generation

## TDD Workflow

```bash
# 1. Generate test vectors (Python oracle)
cd pyscripts && uv sync
uv run python -m pyscripts.generate_vectors

# 2. Write Rust test that loads test_vectors/*.json — should FAIL (RED)
cargo test --workspace

# 3. Implement — should PASS (GREEN)
cargo test --workspace

# 4. Lint
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

### Python Oracle Strategy

For SeedLink, the Python oracle can:
- **Capture real traffic**: Connect to IRIS/GEOFON SeedLink servers, record raw TCP bytes
- **Parse with ObsPy**: `obspy.clients.seedlink` provides reference client implementation
- **Generate protocol vectors**: Known command/response pairs for parser testing
- **Validate frames**: Decode captured SeedLink frames and extract miniSEED records

Test vectors are JSON files in `pyscripts/test_vectors/` (gitignored).

Example vector structure:
```json
{
  "description": "HELLO handshake with IRIS server",
  "client_sends": "HELLO\r\n",
  "server_responds": "SeedLink v3.1 (2020.075)\r\n...\r\n",
  "parsed": {
    "version": "3.1",
    "software": "SeedLink",
    "organization": "IRIS"
  }
}
```

## SeedLink Key Concepts

### Frame Format (v3)
- 8-byte header: "SL" (2 bytes) + sequence number (6 hex chars, ASCII)
- 512-byte payload: standard miniSEED v2 record
- Total: 520 bytes per frame

### State Machine
```
IDLE → HELLO → CONFIGURING (STATION/SELECT) → STREAMING (DATA/END) → BYE
```

### Sequence Numbers
- 6 hex digits (000000-FFFFFF), wrapping
- Client can resume from last known sequence to avoid data loss
- Server maintains per-client cursor into ring buffer

## References

- SeedLink v3 protocol: https://ds.iris.edu/ds/nodes/dmc/services/seedlink/
- SeedLink v4 (draft): https://docs.fdsn.org/projects/seedlink/
- libslink (C reference): https://github.com/EarthScope/libslink
- ObsPy SeedLink client: https://docs.obspy.org/packages/obspy.clients.seedlink.html
- miniseed-rs: https://github.com/luhtfiimanal/miniseed-rs
