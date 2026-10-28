# Changelog

## Unreleased

- `Deflate`, `Zlib` and `ZlibUnterminated` decode with the `zlib-rs` crate (a direct dependency, used through its own
  streaming `Inflate`) instead of flate2's miniz_oxide backend: the same bytes and the same refusals (the codec tests
  and the real-data corpus hold it), faster. Compression stays on flate2 with miniz_oxide, so every compressed byte
  this crate writes is unchanged; flate2's backend is not switched, which would also have switched the other crates
  that depend on flate2 in the same build.

## 0.2.1 (2026-10-27)

- `compress::oodle_framing::walk` walks an Oodle stream's block and quantum headers without decoding it and checks that they end exactly at the end of the payload and the decoded size, reporting per stream its blocks, decoder types, restart bits and quanta (`Framing`). The library's one-shot decode and oozextract do not notice bytes left after the last block; `Oodle::decompress` is unchanged, and a container that knows its payload's exact extent calls `walk` first.

## 0.2.0 (2026-10-27)

- Oodle off Windows is decoded by upstream `oozextract` 0.5.7, which has the single-symbol Huffman and LZB16 fixes
  the `primase-oozextract` fork carried; the fork is retired. `compress::reference::Failure::Refused` now carries
  0.5.7's `OozError` (a breaking change, hence 0.2.0).

## 0.1.0 (2026-10-27)

- First release: checked byte readers, game-file hashes, vertex decoding, texture sampling, and the LZ4, deflate, zstd, LZO2A and Oodle codecs game files use.
