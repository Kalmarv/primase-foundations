# Changelog

## Unreleased

- `compress::oodle_framing::walk` walks an Oodle stream's block and quantum headers without decoding it and checks that they end exactly at the end of the payload and the decoded size, reporting per stream its blocks, decoder types, restart bits and quanta (`Framing`). The library's one-shot decode and oozextract do not notice bytes left after the last block; `Oodle::decompress` is unchanged, and a container that knows its payload's exact extent calls `walk` first.

## 0.2.0 (2026-10-27)

- Oodle off Windows is decoded by upstream `oozextract` 0.5.7, which has the single-symbol Huffman and LZB16 fixes
  the `primase-oozextract` fork carried; the fork is retired. `compress::reference::Failure::Refused` now carries
  0.5.7's `OozError` (a breaking change, hence 0.2.0).

## 0.1.0 (2026-10-27)

- First release: checked byte readers, game-file hashes, vertex decoding, texture sampling, and the LZ4, deflate, zstd, LZO2A and Oodle codecs game files use.
