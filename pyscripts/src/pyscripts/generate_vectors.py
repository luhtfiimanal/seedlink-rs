"""Generate test vectors for seedlink-rs TDD."""

import json
import struct
from pathlib import Path

VECTORS_DIR = Path(__file__).parent.parent.parent / "test_vectors"


def generate_sequences() -> list[dict[str, object]]:
    """Generate sequence number test vectors."""
    vectors: list[dict[str, object]] = []

    cases: list[tuple[str, int]] = [
        ("000000", 0),
        ("000001", 1),
        ("00001A", 26),
        ("0000FF", 255),
        ("001000", 4096),
        ("FFFFFF", 0xFFFFFF),
        ("00004F", 79),
        ("ABCDEF", 0xABCDEF),
    ]

    for hex_str, value in cases:
        vectors.append(
            {
                "v3_hex": hex_str,
                "value": value,
                "v4_decimal": str(value),
            }
        )

    return vectors


def generate_commands() -> list[dict[str, object]]:
    """Generate command parse/serialize test vectors."""
    vectors: list[dict[str, object]] = []

    # Each vector: line, parsed fields, v3_wire, v4_wire (if valid)
    test_cases: list[dict[str, object]] = [
        {
            "line": "HELLO",
            "command": "Hello",
            "fields": {},
            "v3_wire": "HELLO\r\n",
            "v4_wire": "HELLO\r\n",
        },
        {
            "line": "STATION ANMO IU",
            "command": "Station",
            "fields": {"station": "ANMO", "network": "IU"},
            "v3_wire": "STATION ANMO IU\r\n",
            "v4_wire": "STATION IU_ANMO\r\n",
        },
        {
            "line": "SELECT ??.BHZ",
            "command": "Select",
            "fields": {"pattern": "??.BHZ"},
            "v3_wire": "SELECT ??.BHZ\r\n",
            "v4_wire": "SELECT ??.BHZ\r\n",
        },
        {
            "line": "DATA",
            "command": "Data",
            "fields": {"sequence": None, "start": None, "end": None},
            "v3_wire": "DATA\r\n",
            "v4_wire": "DATA\r\n",
        },
        {
            "line": "DATA 00001A",
            "command": "Data",
            "fields": {"sequence": 26, "start": None, "end": None},
            "v3_wire": "DATA 00001A\r\n",
            "v4_wire": "DATA 26\r\n",
        },
        {
            "line": "END",
            "command": "End",
            "fields": {},
            "v3_wire": "END\r\n",
            "v4_wire": "END\r\n",
        },
        {
            "line": "BYE",
            "command": "Bye",
            "fields": {},
            "v3_wire": "BYE\r\n",
            "v4_wire": "BYE\r\n",
        },
        {
            "line": "INFO ID",
            "command": "Info",
            "fields": {"level": "ID"},
            "v3_wire": "INFO ID\r\n",
            "v4_wire": "INFO ID\r\n",
        },
        {
            "line": "BATCH",
            "command": "Batch",
            "fields": {},
            "v3_wire": "BATCH\r\n",
            "v4_wire": None,  # v3 only
        },
        {
            "line": "CAT",
            "command": "Cat",
            "fields": {},
            "v3_wire": "CAT\r\n",
            "v4_wire": None,  # v3 only
        },
        {
            "line": "TIME 2024,1,15,0,0,0",
            "command": "Time",
            "fields": {"start": "2024,1,15,0,0,0", "end": None},
            "v3_wire": "TIME 2024,1,15,0,0,0\r\n",
            "v4_wire": None,  # v3 only
        },
        {
            "line": "SLPROTO 4.0",
            "command": "SlProto",
            "fields": {"version": "4.0"},
            "v3_wire": None,  # v4 only
            "v4_wire": "SLPROTO 4.0\r\n",
        },
        {
            "line": "ENDFETCH",
            "command": "EndFetch",
            "fields": {},
            "v3_wire": None,  # v4 only
            "v4_wire": "ENDFETCH\r\n",
        },
    ]

    vectors.extend(test_cases)
    return vectors


def generate_responses() -> list[dict[str, object]]:
    """Generate response parse test vectors."""
    return [
        {"line": "OK", "type": "Ok"},
        {"line": "END", "type": "End"},
        {
            "line": "ERROR",
            "type": "Error",
            "code": None,
            "description": "",
        },
        {
            "line": "ERROR UNSUPPORTED unknown command",
            "type": "Error",
            "code": "UNSUPPORTED",
            "description": "unknown command",
        },
        {
            "line": "ERROR UNAUTHORIZED access denied",
            "type": "Error",
            "code": "UNAUTHORIZED",
            "description": "access denied",
        },
        {
            "line": "ERROR something went wrong",
            "type": "Error",
            "code": None,
            "description": "something went wrong",
        },
    ]


def generate_v3_frames() -> list[dict[str, str | int]]:
    """Generate v3 frame test vectors (520 bytes)."""
    vectors: list[dict[str, str | int]] = []

    cases: list[tuple[int, bytes]] = [
        (0, bytes(512)),
        (26, bytes([0xAA] * 512)),
        (0xFFFFFF, bytes(range(256)) * 2),
        (79, bytes([0x42] * 512)),
    ]

    for seq_val, payload in cases:
        seq_hex = f"{seq_val:06X}"
        frame = b"SL" + seq_hex.encode("ascii") + payload
        vectors.append(
            {
                "frame_hex": frame.hex(),
                "sequence": seq_val,
                "payload_hex": payload.hex(),
            }
        )

    return vectors


def generate_v4_frames() -> list[dict[str, object]]:
    """Generate v4 frame test vectors (variable length)."""
    vectors: list[dict[str, object]] = []

    # format_byte, subformat_byte, sequence, station_id, payload
    cases: list[tuple[int, int, int, str, bytes]] = [
        (ord("2"), ord("D"), 42, "IU_ANMO", b"test payload"),
        (ord("3"), ord("E"), 0, "", b""),
        (ord("J"), ord("I"), 999, "GE_WLF_00_BHZ", b'{"info":"test"}'),
        (ord("X"), ord("R"), 0xFFFFFFFFFFFFFFFF, "X", b"<xml/>"),
    ]

    for fmt_byte, subfmt_byte, seq_val, station_id, payload in cases:
        station_bytes = station_id.encode("ascii")
        header = struct.pack(
            "<2sBBIQ B",
            b"SE",
            fmt_byte,
            subfmt_byte,
            len(payload),
            seq_val,
            len(station_bytes),
        )
        frame = header + station_bytes + payload

        vectors.append(
            {
                "frame_hex": frame.hex(),
                "format_byte": chr(fmt_byte),
                "subformat_byte": chr(subfmt_byte),
                "sequence": seq_val,
                "station_id": station_id,
                "payload_hex": payload.hex(),
                "total_len": len(frame),
            }
        )

    return vectors


def write_json(name: str, data: object) -> None:
    """Write JSON test vector file."""
    path = VECTORS_DIR / f"{name}.json"
    path.write_text(json.dumps(data, indent=2) + "\n")
    print(f"  wrote {path} ({path.stat().st_size} bytes)")


def main() -> None:
    VECTORS_DIR.mkdir(parents=True, exist_ok=True)
    print(f"Generating test vectors in {VECTORS_DIR}/")

    write_json("sequences", generate_sequences())
    write_json("commands", generate_commands())
    write_json("responses", generate_responses())
    write_json("v3_frames", generate_v3_frames())
    write_json("v4_frames", generate_v4_frames())

    print("Done!")


if __name__ == "__main__":
    main()
