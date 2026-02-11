# CLAUDE.md — seedlink-rs

Pure Rust SeedLink client and server. Zero unsafe, zero C dependency. Apache 2.0.

Sister project of [miniseed-rs](https://github.com/luhtfiimanal/miniseed-rs).

## CRITICAL

- **Diskusi dulu sebelum implementasi** — investigasi, jelaskan, diskusikan, baru code
- **Jangan push tanpa persetujuan user**
- **stdout workaround**: `script -q -c "cargo test --workspace" /dev/null` (Claude Code bug)
- **Zero unsafe** — no FFI, no transmute, no raw pointers

## Scope

**SeedLink v3** real-time seismic data streaming protocol:

- **Protocol**: SeedLink command parsing, frame format, handshake
- **Client**: Async TCP client (tokio) — connect to IRIS/BMKG/GEOFON servers
- **Server**: Async TCP server (tokio) — distribute miniSEED records to clients
- **Integration**: Uses `miniseed-rs` for miniSEED record decode/encode

## Workspace Structure

```
seedlink-rs/           # Cargo workspace
  seedlink-protocol/   # Shared: commands, frames, parsing
    src/lib.rs
  seedlink-client/     # Async client (tokio)
    src/lib.rs
  seedlink-server/     # Async server (tokio)
    src/lib.rs
  pyscripts/           # TDD oracle (uv + ruff + basedpyright)
```

## Commands

```bash
cargo build --workspace                # build all
cargo test --workspace                 # test all
cargo test -p seedlink-protocol        # test single crate
cargo clippy --workspace -- -D warnings  # lint (strict)
cargo fmt -- --check                   # format check

# pyscripts (TDD vector generation)
cd pyscripts && uv sync
cd pyscripts && uv run python -m pyscripts.generate_vectors
cd pyscripts && uv run ruff check src
cd pyscripts && uv run basedpyright src
```

## TDD Strategy

Python scripts capture/generate SeedLink protocol exchanges → Rust tests assert against them.

1. `cd pyscripts && uv run python -m pyscripts.generate_vectors`
2. Write Rust test loading `test_vectors/*.json` — RED
3. Implement Rust code — GREEN
4. Validate: protocol parsing matches captured traffic exactly

Test vectors saved as JSON in `pyscripts/test_vectors/` (gitignored, regenerate locally).

## Code Quality

- `cargo fmt` + `cargo clippy --workspace -- -D warnings` — pre-commit enforced
- `thiserror` for all error types
- No `unsafe` anywhere
- pyscripts: `basedpyright` strict + `ruff`

## SeedLink Protocol Overview

SeedLink is a TCP-based protocol for real-time seismic data streaming.

### Connection Flow

```
Client                          Server
  |--- HELLO ------------------>|
  |<-- SeedLink vX.Y ... ------|
  |--- STATION STA NET -------->|
  |--- SELECT ??.BHZ ---------->|
  |--- DATA -------------------->|
  |<-- SL record (miniSEED) ----|
  |<-- SL record (miniSEED) ----|
  |--- BYE --------------------->|
```

### SeedLink v3 Frame Format

```
Bytes 0-7:    SeedLink header
              ├─ [0..2]    "SL" signature
              ├─ [2..8]    Sequence number (6 hex digits)
Bytes 8-519:  miniSEED v2 record (512 bytes)
```

Total frame: 520 bytes (8-byte SL header + 512-byte miniSEED)

### Key Commands

| Command | Description |
|---------|-------------|
| `HELLO` | Handshake, server returns version info |
| `STATION sta net` | Select station and network |
| `SELECT pattern` | Select channels (e.g., `??.BHZ`) |
| `DATA` | Start streaming from beginning |
| `DATA seq` | Resume from sequence number |
| `TIME start end` | Request time window |
| `END` | End channel selection, start transfer |
| `BYE` | Close connection |
| `INFO level` | Request server info (ID, STATIONS, STREAMS, etc.) |

## References

- SeedLink v3 protocol: https://ds.iris.edu/ds/nodes/dmc/services/seedlink/
- SeedLink v4 (draft): https://docs.fdsn.org/projects/seedlink/
- libslink (C reference): https://github.com/EarthScope/libslink
- IRIS SeedLink servers: rtserve.iris.washington.edu:18000
- GEOFON SeedLink: geofon.gfz-potsdam.de:18000
