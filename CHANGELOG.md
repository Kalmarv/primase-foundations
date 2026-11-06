# Changelog

## 0.3.4 (2026-11-06)

- `compress::Lzma`: LZMA as Unity's asset bundles and 7-Zip's packed streams store it, the five property bytes and the
  raw stream with no 8-byte size, decoded through `lzma-rs` 0.3.0 to the size the caller expects. A stream may end
  without the end-of-payload marker (Unity, 7-Zip) or with it (liblzma); input left over after the expected size is
  accepted only when it is that marker. A shorter or longer payload, a cut stream and fewer than five property bytes are
  refused. Fixtures from p7zip and CPython's liblzma, with the script that makes them.

## 0.3.3 (2026-11-01)

- `scan` bounds its memory. An extent longer than a window is a large read of its own; the large reads go first,
  longest first, but those in flight hold at most four windows a thread between them, and one longer than that is read
  only while no other large read is held. Threads that cannot take the next large read take ordinary windows meanwhile,
  and a large read's buffer is freed once its extent is visited. The scan holds about `threads` windows plus the larger
  of four windows a thread and its largest extent; before, each thread kept a buffer as large as the largest extent it
  had read, and the largest reads started together.

## 0.3.2 (2026-11-01)

- `scan` (Unix and Windows): many stored entries of a few large files read in the files' own order, for the
  whole-archive pass of a census, an index build or an extraction. The wanted entries are sorted by file and offset and
  joined into windows (16 MiB by default, gaps of at most 1 MiB); each thread reads a window in one call and hands its
  entries to the caller's function on the same thread, mutable for decryption in place, so decoding overlaps the next
  reads. On Windows the files are opened unbuffered. An entry that cannot be read reaches the function as its
  `ExtentError` and the scan goes on; a panic in the function stops it and returns as `parallel::Panicked`. Measured on
  a full Elden Ring install (97,118 entries, 52 GB stored, decryption and decompression in the function, 16 threads):
  9.0 s, against 23.2 s entry by entry through the file cache. `scan` is the crate's second stated exception to the
  no-file-access rule: it opens and reads the files its caller names.

## 0.3.1 (2026-10-31)

- `sqlite` (feature `sqlite`): the tables of an SQLite 3 file image read whole through `quantydb-sqlite` 0.4.0, with
  strict refusals (a write-ahead log, a virtual or `WITHOUT ROWID` table, a row of the wrong width, a b-tree that
  reaches a page twice or lists rowids out of order, invalid text, a truncated file). Moved here from rme-formats,
  where it read Tony Hawk: SHRED's databases, unchanged, so that other game readers can use it; its tests and their
  Python sqlite3 fixture moved with it.

- `compress::Lzo1x`: LZO1X decoding (`lzo1x_decompress_safe` of LZO 2.10, the format miniLZO writes; Nadeo's Gbx
  files store their bodies in it), strict as the other codecs, with `LZO1X_MAX_RATIO` bounding a claimed size by
  the input. Checked against hand-worked streams and streams written by LZO 2.10's own compressors (python-lzo).
- `compress::Inflater`: a streaming inflater for zlib and raw DEFLATE that takes its input in pieces with a sync flush
  and reports what each call consumed and produced, so a reader can interleave inflating with producing the input
  (pack files that decrypt 256 bytes at a time).

## 0.3.0 (2026-10-29)

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
