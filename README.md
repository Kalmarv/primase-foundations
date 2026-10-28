# primase-foundations

Checked byte reading, game-file hashes, vertex decoding, texture sampling and the compression codecs games store their data in: the shared building blocks for writing a reader or converter for a game's files.

It was written for readers of Frostbite (skate.), RAGE (Grand Theft Auto V), Anvil (Riders Republic) and FromSoftware (Sekiro) data, so its hashes, codecs and layout conventions are the ones those games use: Grand Theft Auto V's Jenkins name hashes, Sekiro's zlib and Oodle streams, skate.'s zlib blocks, Riders Republic's LZ4 blocks, DXGI vertex formats, DXBC's MD5 checksum. None of it is specific to a game, and it reads no game's container format itself; it is the layer a container reader sits on.

## What is in it

| module | what it gives you |
| --- | --- |
| `bytes` | `View` and `Cursor` over a borrowed slice. Every multi-byte read states its byte order; a refused read reports the absolute file offset; `Cursor::count` checks that a stored element count fits in the rest of the data before you allocate for it. |
| `hash` | CRC-32, Adler-32 from any start value, Jenkins one-at-a-time (case-folded and exact), MD5 with its block function exposed, SDBM. Inputs are never normalized. |
| `vertex` | DXGI vertex element formats (float, half, SNORM/UNORM 8/10/16-bit), layouts with explicit or appended offsets, stream decoding, packed 11/11/10 and 21/21/22 positions, packed tangents, strip-to-list conversion, index widths, splitting a mesh into 16-bit-indexed pieces. The `f32` operation order is fixed and not fused. |
| `parallel` | An ordered parallel `map` and an in-place `rows_mut` over scoped threads, with the thread count passed in. Results come back in item order and equal the serial run; a panicking item becomes an error naming it. |
| `color` | The sRGB transfer curve in `f64` and `f32`, and its byte forms (a 256-entry table one way, a table of rounding thresholds the other). |
| `sampling` | Texture sampling as GPUs define it: wrap, clamp, mirror and mirror-once addressing; point, bilinear and trilinear filtering; level of detail from a footprint; 2D and 3D; optional Direct3D 11 sub-texel precision. |
| `compress` | LZ4 blocks, raw deflate, zlib (also with a missing end marker), Zstandard, LZO2A and Oodle behind one `Decompressor`/`Compressor` interface. |

## Example

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

The same example is the crate-level documentation's, and `cargo test` compiles and runs it.

## Features

| feature | default | what it enables | what it pulls in |
| --- | --- | --- | --- |
| *(none)* | yes | `bytes`, `hash`, `vertex`, `parallel`, `color`, `sampling` | `thiserror`, `half` |
| `compress` | no | the `compress` module: LZ4, deflate, zlib, Zstandard, LZO2A, Oodle | `lz4_flex`, `flate2` (pure-Rust backend), `ruzstd`, `libloading`, `oozextract` |
| `reference-decoder` | no | on Windows, `compress::reference`: oozextract exposed as a second Oodle decoder, for comparing the game's library against it (off Windows it is always there under `compress`) | `compress` |

### Compression

Each codec decodes with `Decompressor::decompress(input, expected_len)`. The expected output size comes from the container, and a stream that produces more, fewer, or has input left over is refused, as is one that ends early or fails a checksum the format carries. The error names the codec and the input offset. There is no format autodetection. Encoding is available for LZ4, deflate and zlib, and for Oodle (Kraken) through the game's library.

| codec | type | decode | encode |
| --- | --- | --- | --- |
| LZ4 raw block | `Lz4` | yes | yes |
| raw deflate | `Deflate` | yes | yes |
| zlib | `Zlib`, `ZlibUnterminated` | yes | `Zlib` only |
| Zstandard frame | `Zstd` | yes | no |
| LZO2A | `Lzo2a` | yes | no |
| Oodle | `oodle::Oodle` | yes | Kraken, with the game's library only |

**Oodle.** `oodle_framing::walk` reads an Oodle stream's block and quantum headers without decoding it and checks that they end exactly at the end of the payload and the decoded size, because the library's one-shot decode does not say whether bytes were left over. The library and oozextract decode a stream with a byte added after it; a container that knows its payload's exact extent calls `walk` first.

The Oodle library is not shipped with this crate and is not downloaded. Point `Oodle::load` at the copy the game itself ships (`oo2core_5_win64.dll`, `oo2core_6_win64.dll` or `oo2core_9_win64.dll`, for the `Version` that matches); it loads exactly that path and is `unsafe`, because loading a native library runs its initialization code. Off Windows, `Oodle::open(None, version)` decodes with the pure-Rust [oozextract](https://github.com/lvlvllvlvllvlvl/oozextract) instead, which refuses a stream it cannot decode and cannot compress. On Windows there is no built-in decoder, so a library path is required.

## What it guarantees

- The byte readers, the vertex decoders and the codecs return errors on malformed input instead of panicking. Offsets, counts and sizes are checked for overflow before they are used in them.
- A refused read says where: `bytes` errors carry the absolute file offset (even where adding the origin overflows `usize`), `FieldError` names the field, and codec errors name the codec and the input offset.
- `Levels` has public fields, so every sampling method checks the level it reads and refuses one that is missing or does not hold its size's bytes; `parallel::rows_mut` refuses a row length of zero.
- A failed `Cursor` read or seek leaves the cursor where it was.
- Decoding is exact rather than lenient: no format autodetection, no implicit normalization of hash inputs, no implicit coordinate-axis changes, and `f32` arithmetic in a documented order without fused multiply-add, so the same bytes give the same bits on every platform.
- The library modules do no file, environment or process access. The one place a path is touched is `Oodle::load`, which canonicalizes the path it is given.
- `unsafe` code is denied crate-wide except in `compress::oodle`, where it calls into the loaded library.

## What it does not do

- It does not read any game's archive or container format; it has no notion of Sekiro's DCX files, Frostbite's CAS or GTA V's RPF. Those readers sit on top of it.
- It does not decrypt anything.
- It does not ship, download or search for the Oodle library. Without the game's copy, Oodle compression is unavailable (oozextract only decodes).
- A claimed output size is bounded by the input before anything is allocated: LZ4, deflate, zlib and LZO2A refuse a claim above `LZ4_MAX_RATIO`, `DEFLATE_MAX_RATIO` or `LZO2A_MAX_RATIO` times the input, and zstd grows its output with what the stream produces. Oodle has no proven ratio, so its decode allocates the claimed size up front (fallibly, as `Error::Limit`): bound a size read from a hostile header first. `Deflate::decompress_up_to` decodes with a maximum instead of an exact size.
- The Oodle library, when loaded, is native code: this crate cannot vouch for what it does with a hostile stream.

## Tests

```sh
cargo test --workspace   # synthetic tests; no game files needed (the dev-dependencies turn the codec features on)
```

Unit and integration tests use hand-worked values, published test vectors (RFC 1321 for MD5, the Adler-32 "Wikipedia" example, the CRC-32 check value) and small fixtures generated by independent programs (`tests/fixtures/vertex`, produced by a separate C# implementation of the arithmetic; LZO2A streams laid out by hand from LZO 2.10's source).

There are also **real-data tests** that read your own game files; they are skipped, each with a line naming what is missing, unless you opt in. Set the environment variable `GAME_DATA` to the absolute path of a TOML file outside the repository that says where your files are:

```toml
[tools]
# the Oodle libraries your games ship, to compare them against oozextract
oodle6 = 'D:/games/Sekiro/oo2core_6_win64.dll'

[oracles]
# <root>/<set>/<game>/<file>: here <root>/corpus/general/codecs.tsv
root = 'D:/oracles'

[scratch]
root = 'D:/scratch'

[run]
require = 'gate'   # inputs the tests mark as required fail, rather than skip, when unset
```

Every section and key is optional and every value is a string. The keys this crate reads are `tools.oodle5`, `tools.oodle6`, `tools.oodle9`, `oracles.root` (for the codec corpus `corpus/general/codecs.tsv`, made by `tests/oracles/codecs.py` from streams cut out of your games, as its header describes) and `scratch.root`; [tests/inputs/README.md](tests/inputs/README.md) lists each with what it is and where it comes from. The file format itself is that of [primase-testdata](https://github.com/Kalmarv/primase-testdata), the test-support crate that reads it. Oodle tests are Windows-only (off Windows oozextract decodes and nothing is configured).

Building needs a Rust toolchain of at least 1.99 (edition 2024); `rust-toolchain.toml` pins 1.99.0.

The asset-conversion library [primase](https://github.com/Kalmarv/primase) is built on this crate.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). Dependencies keep their own licenses; in particular oozextract, which this crate decodes Oodle with, is MIT-licensed.
