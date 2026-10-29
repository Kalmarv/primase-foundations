# Changelog

## Unreleased

- `oodle::Level` and `Oodle::compress_at`: compress at a named level (a DCX header's level byte); `compress_as`
  stays at level 4.
- `sampling`: border addressing. `Address` gains `Border` (an index outside the texture reads no texel), and every
  filter (`Levels::bilinear`, `point`, `trilinear`; `Decoded::bilinear`, `bilinear_at`, `point`, `point_3d`,
  `linear_3d`) takes `impl Into<Addressing<N>>`: the axes' addresses and the colour a border reads, taken as it is (no
  transfer). An array of addresses converts to addressing with no border colour, so a caller passing `[Address; N]` is
  unchanged; a border axis with no colour, or a colour that isn't finite, is refused. `Levels::texel` refuses a
  coordinate outside a border axis. Breaking: the filters' address parameter is generic, and a `match` on `Address`
  needs the new arm.

- `Deflate`, `Zlib` and `ZlibUnterminated` decode with the `zlib-rs` crate (a direct dependency, used through its own
  streaming `Inflate`) instead of flate2's miniz_oxide backend: the same bytes and the same refusals (the codec tests
  and the real-data corpus hold it), faster. Compression stays on flate2 with miniz_oxide, so every compressed byte
  this crate writes is unchanged; flate2's backend is not switched, which would also have switched the other crates
  that depend on flate2 in the same build. A raw DEFLATE stream's window starts zero-filled, as miniz_oxide's did, so
  a stream that copies from before its first byte reads zeros there instead of being refused (GTA V Enhanced stores
  one; zlib refuses it as "invalid distance too far back"); zlib streams still start with an empty window.
- `bytes::View`'s small reads (`slice`, `array`, `u8`, the integer and float reads) and `checked_span` are `#[inline]`.

## 0.2.1 (2026-10-27)

- `compress::oodle_framing::walk` walks an Oodle stream's block and quantum headers without decoding it and checks that they end exactly at the end of the payload and the decoded size, reporting per stream its blocks, decoder types, restart bits and quanta (`Framing`). The library's one-shot decode and oozextract do not notice bytes left after the last block; `Oodle::decompress` is unchanged, and a container that knows its payload's exact extent calls `walk` first.

## 0.2.0 (2026-10-27)

- Oodle off Windows is decoded by upstream `oozextract` 0.5.7, which has the single-symbol Huffman and LZB16 fixes
  the `primase-oozextract` fork carried; the fork is retired. `compress::reference::Failure::Refused` now carries
  0.5.7's `OozError` (a breaking change, hence 0.2.0).

## 0.1.0 (2026-10-27)

- First release: checked byte readers, game-file hashes, vertex decoding, texture sampling, and the LZ4, deflate, zstd, LZO2A and Oodle codecs game files use.
