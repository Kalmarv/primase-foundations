# primase-foundations

A Rust library of shared building blocks for programs that read and convert game files. It is the base layer of [primase](https://github.com/Kalmarv/primase) and the format crates built on it, and it does not read any game's archive or container format itself.

## Features

- Bounds-checked `View` and `Cursor` byte readers with the byte order stated on every multi-byte read
- CRC-32, Adler-32 from any start value, Jenkins one-at-a-time (case-folded and exact), MD5 with its block function exposed, and SDBM
- Decoding of ten DXGI vertex element formats, vertex layouts and vertex streams
- Packed 11/11/10 and 21/21/22 positions, packed tangents, triangle strip to list conversion, and mesh splitting for 16-bit indices
- Texture sampling with wrap, clamp, mirror, mirror-once and border addressing; point, bilinear and trilinear filtering; 2D and 3D textures
- The sRGB transfer curve in `f64` and `f32`, with table-based byte conversions
- An ordered parallel `map` and in-place `rows_mut` over scoped threads with a stated thread count
- `scan`, which reads many entries of a few large archive files in file order on several threads (unbuffered on Windows)
- Decompression of LZ4 blocks, raw deflate, zlib, Zstandard, LZO1X, LZO2A and Oodle, plus a streaming zlib/deflate inflater (feature `compress`)
- Compression to LZ4, deflate and zlib, and to Oodle Kraken through the game's own Oodle library
- A walker that checks an Oodle stream's block and quantum headers without decoding it
- Reading whole tables from an SQLite 3 file image (feature `sqlite`)
- Malformed input returns an error naming the offset instead of panicking, and decoders allocate no more than the input can produce (Oodle excepted)
- Vertex arithmetic uses a fixed `f32` operation order without fused multiply-add, so the same bytes give the same bits on every platform
- Library code does no file, environment or process access, except `Oodle::load` and `scan`; `unsafe` is used only in `compress::oodle`

## Getting Started

### Requirements

Rust 1.99 or newer (edition 2024). `rust-toolchain.toml` pins 1.99.0.

### Installation

The crate is published on [crates.io](https://crates.io/crates/primase-foundations).

```sh
cargo add primase-foundations
cargo add primase-foundations --features compress,sqlite
```

| feature | what it adds |
| --- | --- |
| *(default)* | `bytes`, `hash`, `vertex`, `parallel`, `scan` (Unix and Windows), `color`, `sampling` |
| `compress` | the `compress` module: LZ4, deflate, zlib, Zstandard, LZO1X, LZO2A and Oodle |
| `sqlite` | the `sqlite` module, through `quantydb-sqlite` |
| `reference-decoder` | on Windows, `compress::reference`: oozextract as a second Oodle decoder for comparing against the game's library |

The Oodle library is not shipped or downloaded. `Oodle::load` takes the path of the DLL the game ships (`oo2core_5_win64.dll`, `oo2core_6_win64.dll` or `oo2core_9_win64.dll`) and is `unsafe` because loading it runs native code. Off Windows, `Oodle::open(None, version)` decodes with the pure-Rust oozextract instead, which cannot compress; on Windows a library path is required.

### Usage

Read a small record with a `Cursor`, then see what a truncated copy reports:

```rust
use primase_foundations::bytes::{Cursor, Endian, Field};
use primase_foundations::hash::crc32;

// "DATA", a little-endian count of 2-byte entries, then the entries
let file = [b'D', b'A', b'T', b'A', 2, 0, 0, 0, 0x34, 0x12, 0x78, 0x56];

let mut cursor = Cursor::new(&file);
assert_eq!(cursor.take(4)?, b"DATA");
let count = cursor.count(Endian::Little, 2)?; // refused unless that many entries fit
let mut entries = Vec::new();
for _ in 0..count {
    entries.push(cursor.u16(Endian::Little)?);
}
assert_eq!(entries, [0x1234, 0x5678]);
assert_eq!(crc32(b"123456789"), 0xcbf4_3926);

// cut short, the read is refused with the field and the offset
let mut cut = Cursor::new(&file[..6]);
cut.take(4)?;
let refusal = cut.count(Endian::Little, 2).field("entry count").unwrap_err();
assert_eq!(refusal.to_string(), "entry count at 0x4: 4 bytes run past the end of 6");
Ok::<(), Box<dyn std::error::Error>>(())
```

With `compress` enabled, every codec decodes through `Decompressor::decompress(input, expected_len)`, where the expected size comes from the container; any other output size, leftover input or failed checksum is an error. There is no format autodetection. [docs/guarantees.md](docs/guarantees.md) lists what the crate guarantees and what it does not do.

Run the tests with `cargo test --workspace`; they need no game files. Opt-in tests that read your own game files are described in [docs/testing.md](docs/testing.md), and build and contribution rules are in [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under GPL-3.0-or-later; see [LICENSE](LICENSE).
