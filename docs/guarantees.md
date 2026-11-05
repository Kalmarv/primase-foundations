# Guarantees and limits

## Codecs

| codec | type | decode | encode |
| --- | --- | --- | --- |
| LZ4 raw block | `Lz4` | yes | yes |
| raw deflate | `Deflate` | yes | yes |
| zlib | `Zlib`, `ZlibUnterminated` | yes | `Zlib` only |
| zlib or raw deflate, fed piece by piece | `Inflater` | yes | no |
| Zstandard frame | `Zstd` | yes | no |
| LZO1X (miniLZO's format) | `Lzo1x` | yes | no |
| LZO2A | `Lzo2a` | yes | no |
| Oodle | `oodle::Oodle` | yes | Kraken, with the game's library only |

Each codec decodes with `Decompressor::decompress(input, expected_len)`. The expected output size comes from the container, and a stream that produces more or fewer bytes, has input left over, ends early or fails a checksum the format carries is refused. The error names the codec and the input offset. `Deflate::decompress_up_to` decodes with a maximum instead of an exact size. `Inflater` takes its input in pieces with a sync flush and reports what each call consumed and produced.

A raw deflate stream's window starts zero-filled, so a stream that copies from before its first byte reads zeros there. zlib streams start with an empty window.

`oodle::Oodle::compress_at` compresses at a named `oodle::Level` (-4 to 9); `compress_as` and `Compressor::compress` use level 4.

`oodle_framing::walk` reads an Oodle stream's block and quantum headers without decoding it and checks that they end exactly at the end of the payload and the decoded size. The Oodle library and oozextract both decode a stream with a byte added after it, so a container that knows its payload's exact extent calls `walk` first.

## Guarantees

- The byte readers, the vertex decoders and the codecs return errors on malformed input instead of panicking. Offsets, counts and sizes are checked for overflow before they are used.
- A refused read says where: `bytes` errors carry the absolute file offset (even where adding the origin overflows `usize`), `FieldError` names the field, and codec errors name the codec and the input offset.
- A failed `Cursor` read or seek leaves the cursor where it was.
- `Levels` has public fields, so every sampling method checks the level it reads and refuses one that is missing or does not hold its size's bytes. `parallel::rows_mut` refuses a row length of zero.
- Decoding is exact: no format autodetection, no implicit normalization of hash inputs, no implicit coordinate-axis changes, and `f32` vertex arithmetic in a documented order without fused multiply-add.
- The library modules do no file, environment or process access, with two exceptions: `Oodle::load` canonicalizes the path it is given, and `scan` opens and reads the files its caller names.
- `unsafe` code is denied crate-wide except in `compress::oodle`, where it calls into the loaded library.

## Limits

- It does not read any game's archive or container format, such as Sekiro's DCX files, Frostbite's CAS or GTA V's RPF. Those readers sit on top of it.
- It does not decrypt anything.
- It does not ship, download or search for the Oodle library. Without the game's copy, Oodle compression is unavailable, because oozextract only decodes.
- A claimed output size is bounded by the input before anything is allocated: LZ4, deflate, zlib, LZO1X and LZO2A refuse a claim above `LZ4_MAX_RATIO`, `DEFLATE_MAX_RATIO`, `LZO1X_MAX_RATIO` or `LZO2A_MAX_RATIO` times the input, and zstd grows its output with what the stream produces. Oodle has no proven ratio, so its decode allocates the claimed size up front (fallibly, as `Error::Limit`); bound a size read from an untrusted header first.
- The Oodle library, when loaded, is native code, and this crate cannot vouch for what it does with a hostile stream.
