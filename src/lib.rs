//! Building blocks for reading game files: checked byte reading, the hashes game files key their names by, vertex
//! layouts and attribute decoding, an ordered parallel map, sRGB and texture-sampling arithmetic, and (behind the
//! `compress` feature) the compression codecs games store their data in.
//!
//! The readers and decoders work on borrowed bytes and return a value or an error rather than panicking on malformed
//! input; the library modules do no file, environment or process access (the one exception, loading a native Oodle
//! library the caller names, is in `compress::oodle`). Each module states its conventions (byte order, normalization,
//! rounding) in its own documentation, because file formats differ on exactly those points.
//!
//! - [`bytes`]: bounds-checked [`View`](bytes::View) and [`Cursor`](bytes::Cursor) readers. The byte order is stated
//!   on every multi-byte read, and a refused read reports the absolute offset it was at.
//! - [`hash`]: CRC-32, Adler-32 (from any start value), Jenkins one-at-a-time (case-folded and exact), MD5 with its
//!   block function exposed, and SDBM. Inputs are never normalized.
//! - [`vertex`]: DXGI vertex element formats, layouts with explicit or appended offsets, stream decoding, packed
//!   positions and tangents, and triangle-strip and index-buffer helpers.
//! - [`parallel`]: an ordered, panic-safe parallel map over independent items, with the thread count passed in.
//! - [`color`]: the sRGB transfer curve in `f64` and `f32` and its byte forms.
//! - [`sampling`]: texture sampling as GPUs define it: address modes, point, bilinear and trilinear filtering,
//!   level of detail, 2D and 3D.
//! - `compress` (feature `compress`): one `Decompressor` / `Compressor` interface over LZ4, deflate, zlib, zstd,
//!   LZO2A and Oodle. Every decode takes the exact expected output size and refuses anything else.
//!
//! # Example
//!
//! Read a small made-up record, then see what a truncated copy of it reports:
//!
//! ```
//! use primase_foundations::bytes::{Cursor, Endian, Field};
//! use primase_foundations::hash::crc32;
//!
//! // "DATA", a little-endian count of 2-byte entries, then the entries
//! let file = [b'D', b'A', b'T', b'A', 2, 0, 0, 0, 0x34, 0x12, 0x78, 0x56];
//!
//! let mut cursor = Cursor::new(&file);
//! assert_eq!(cursor.take(4)?, b"DATA");
//! let count = cursor.count(Endian::Little, 2)?; // refused unless that many entries fit
//! let mut entries = Vec::new();
//! for _ in 0..count {
//!     entries.push(cursor.u16(Endian::Little)?);
//! }
//! assert_eq!(entries, [0x1234, 0x5678]);
//! assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
//!
//! // cut short, the read is refused with the field and the offset
//! let mut cut = Cursor::new(&file[..6]);
//! cut.take(4)?;
//! let refusal = cut.count(Endian::Little, 2).field("entry count").unwrap_err();
//! assert_eq!(refusal.to_string(), "entry count at 0x4: 4 bytes run past the end of 6");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#![warn(missing_docs)]
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod bytes;
pub mod color;
#[cfg(feature = "compress")]
pub mod compress;
pub mod hash;
pub mod parallel;
pub mod sampling;
pub mod vertex;

/// This crate's source files as `(name, bytes)`, for a caller that keys a disk cache by the code that computed its
/// values: hashing them retires cached entries when any of them changes. A test keeps the list equal to the `src`
/// folder.
pub const SOURCES: &[(&str, &[u8])] = &[
    ("foundations/bytes.rs", include_bytes!("bytes.rs")),
    ("foundations/color.rs", include_bytes!("color.rs")),
    (
        "foundations/compress/lzo2a.rs",
        include_bytes!("compress/lzo2a.rs"),
    ),
    ("foundations/compress/mod.rs", include_bytes!("compress/mod.rs")),
    (
        "foundations/compress/oodle.rs",
        include_bytes!("compress/oodle.rs"),
    ),
    (
        "foundations/compress/reference.rs",
        include_bytes!("compress/reference.rs"),
    ),
    ("foundations/hash.rs", include_bytes!("hash.rs")),
    ("foundations/lib.rs", include_bytes!("lib.rs")),
    ("foundations/parallel.rs", include_bytes!("parallel.rs")),
    ("foundations/sampling.rs", include_bytes!("sampling.rs")),
    ("foundations/vertex.rs", include_bytes!("vertex.rs")),
];

// The real-data inputs this crate's unit tests read (tests/inputs/mod.rs).
#[cfg(test)]
#[path = "../tests/inputs/mod.rs"]
mod inputs;

/// Makes `cargo test` compile and run the README's example.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
