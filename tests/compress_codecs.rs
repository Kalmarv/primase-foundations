use primase_foundations::compress::*;
use primase_testdata::gate::{GateReport, Tolerance};

/// Literal-only LZ4 and independently generated Python zlib fixtures; round trips
/// include zero, one, and both sides of a 64 KiB boundary.
#[test]
fn fixtures_and_roundtrips() -> Result<()> {
    let mut report = GateReport::new("compression synthetic", Tolerance::Exact);
    report.compare_bytes(
        "LZ4 literal",
        &Lz4.decompress(&[0x50, b'h', b'e', b'l', b'l', b'o'], 5)?,
        b"hello",
    );
    report.compare_bytes(
        "zlib Python",
        &Zlib.decompress(&[120, 156, 203, 72, 205, 201, 201, 7, 0, 6, 44, 2, 21], 5)?,
        b"hello",
    );
    report.compare_bytes(
        "deflate Python",
        &Deflate.decompress(&[203, 72, 205, 201, 201, 7, 0], 5)?,
        b"hello",
    );
    for size in [0, 1, 15, 65535, 65536, 65537, 131072] {
        let input: Vec<u8> = (0..size).map(|i| (i * 31 + i / 17) as u8).collect();
        let codecs: [(&str, &dyn Compressor, &dyn Decompressor); 3] = [
            ("lz4", &Lz4, &Lz4),
            ("deflate", &Deflate, &Deflate),
            ("zlib", &Zlib, &Zlib),
        ];
        for (name, enc, dec) in codecs {
            let encoded = enc.compress(&input)?;
            report.compare_bytes(format!("{name}/{size}"), &dec.decompress(&encoded, size)?, &input);
            assert!(dec.decompress(&encoded, size + 1).is_err());
            if size > 0 {
                assert!(dec.decompress(&encoded, size - 1).is_err());
            }
        }
    }
    println!("{report}");
    report.assert_passed();
    Ok(())
}
#[test]
fn strict_framing() -> Result<()> {
    for codec in [&Deflate as &dyn Decompressor, &Zlib] {
        assert!(codec.decompress(&[], 0).is_err());
    }
    for (enc, dec) in [
        (&Deflate as &dyn Compressor, &Deflate as &dyn Decompressor),
        (&Zlib, &Zlib),
    ] {
        let bytes = enc.compress(b"hello")?;
        for n in 0..bytes.len() {
            assert!(dec.decompress(&bytes[..n], 5).is_err(), "prefix {n}");
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(dec.decompress(&trailing, 5).is_err());
    }
    // Python zlib.compressobj + Z_SYNC_FLUSH: no BFINAL, no Adler trailer.
    let sync = [120, 156, 202, 72, 205, 201, 201, 7, 0, 0, 0, 255, 255];
    assert_eq!(ZlibUnterminated.decompress(&sync, 5)?, b"hello");
    assert!(Zlib.decompress(&sync, 5).is_err());
    let z = Zlib.compress(b"hello")?;
    assert_eq!(ZlibUnterminated.decompress(&z[..z.len() - 4], 5)?, b"hello");
    assert!(ZlibUnterminated.decompress(&z[..z.len() - 4], 6).is_err());
    assert!(ZlibUnterminated.decompress(&[], 0).is_err());
    let mut corrupt = z;
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert!(ZlibUnterminated.decompress(&corrupt, 5).is_err());
    assert!(Lz4.decompress(&[0xf0], 15).is_err());
    Ok(())
}
/// One zstd frame laid out by hand (RFC 8878): single segment with a one-byte content size, then `blocks`, each
/// (last, kind, body): kind 0 raw (body is the bytes), 1 RLE (body is one byte repeated `size` times).
fn zstd_frame(content: u8, dict: bool, blocks: &[(bool, u8, &[u8], u32)]) -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD];
    // descriptor: single segment (0x20), dictionary id flag 1 (one byte) when asked
    f.push(if dict { 0x21 } else { 0x20 });
    if dict {
        f.push(7);
    }
    f.push(content);
    for &(last, kind, body, size) in blocks {
        let header = u32::from(last) | (u32::from(kind) << 1) | (size << 3);
        f.extend_from_slice(&header.to_le_bytes()[..3]);
        f.extend_from_slice(body);
    }
    f
}

/// zstd: raw and RLE blocks decode exactly; a wrong expected size, a stated content size that differs, trailing
/// input, a missing last block, a dictionary and an over-long output are refused.
#[test]
fn zstd_one_exact_frame() -> Result<()> {
    let raw = zstd_frame(5, false, &[(true, 0, b"hello", 5)]);
    assert_eq!(Zstd.decompress(&raw, 5)?, b"hello");
    let two = zstd_frame(8, false, &[(false, 0, b"ab", 2), (true, 1, b"z", 6)]);
    assert_eq!(Zstd.decompress(&two, 8)?, b"abzzzzzz");
    // the frame states 5 bytes
    assert!(Zstd.decompress(&raw, 4).is_err());
    assert!(Zstd.decompress(&raw, 6).is_err());
    let mut trailing = raw.clone();
    trailing.push(0);
    assert!(Zstd.decompress(&trailing, 5).is_err());
    let unfinished = zstd_frame(5, false, &[(false, 0, b"hello", 5)]);
    assert!(Zstd.decompress(&unfinished, 5).is_err());
    let dict = zstd_frame(5, true, &[(true, 0, b"hello", 5)]);
    assert!(Zstd.decompress(&dict, 5).is_err());
    // states 5 but holds 6
    let long = zstd_frame(5, false, &[(true, 0, b"hello!", 6)]);
    assert!(Zstd.decompress(&long, 5).is_err());
    assert!(Zstd.decompress(&raw[..raw.len() - 1], 5).is_err());
    Ok(())
}

/// A raw stream that copies from before its first byte reads zeros there: twelve zeros and "window", compressed by
/// Python's zlib 1.3.1 against a 32 KiB zero dictionary (`compressobj(9, DEFLATED, -15, zdict=bytes(32768))`), which
/// zlib without that dictionary refuses as "invalid distance too far back". A zlib stream's window starts empty.
#[test]
fn a_raw_stream_reads_zeros_before_its_start() -> Result<()> {
    let stream = [67, 6, 229, 153, 121, 41, 249, 229, 0];
    let mut want = vec![0u8; 12];
    want.extend_from_slice(b"window");
    assert_eq!(Deflate.decompress(&stream, want.len())?, want);
    assert_eq!(Deflate.decompress_up_to(&stream, usize::MAX)?, want);
    // the same stream behind a zlib header (78 DA) and its Adler-32 copies from an empty window: refused
    let mut zlib = vec![0x78, 0xDA];
    zlib.extend_from_slice(&stream);
    let adler = want.iter().fold((1u32, 0u32), |(a, b), &x| {
        let a = (a + u32::from(x)) % 65521;
        (a, (b + a) % 65521)
    });
    zlib.extend_from_slice(&(adler.1 << 16 | adler.0).to_be_bytes());
    assert!(Zlib.decompress(&zlib, want.len()).is_err());
    Ok(())
}

/// A stream of unstated size inflates to what it holds, memory following the output and not the bound; more than the
/// bound, a missing end, trailing input and a damaged stream are refused.
#[test]
fn deflate_of_an_unstated_size_is_bounded_by_its_output() -> Result<()> {
    use primase_foundations::compress::DEFLATE_MAX_RATIO;
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let packed = Deflate.compress(&data)?;
    assert_eq!(Deflate.decompress_up_to(&packed, data.len())?, data);
    assert_eq!(Deflate.decompress_up_to(&packed, usize::MAX)?, data);
    assert!(matches!(
        Deflate.decompress_up_to(&packed, data.len() - 1),
        Err(Error::Size { expected, actual }) if expected == data.len() - 1 && actual > expected
    ));
    // a run: the stream is tiny and the output the most it can be
    let zeros = Deflate.compress(&vec![0u8; 1 << 20])?;
    assert!(
        zeros.len() * DEFLATE_MAX_RATIO >= 1 << 20,
        "{} bytes",
        zeros.len()
    );
    assert_eq!(Deflate.decompress_up_to(&zeros, 1 << 20)?.len(), 1 << 20);
    assert!(
        Deflate
            .decompress_up_to(&packed[..packed.len() - 1], usize::MAX)
            .is_err()
    );
    let mut trailing = packed.clone();
    trailing.push(0);
    assert!(Deflate.decompress_up_to(&trailing, usize::MAX).is_err());
    assert!(Deflate.decompress_up_to(&[], usize::MAX).is_err());
    assert!(Deflate.decompress_up_to(&[0xFF; 16], usize::MAX).is_err());
    Ok(())
}

/// Fed input 256 bytes at a time and asked for output 1024 bytes at a time (the pieces a reader that interleaves
/// decoding with other work uses), the inflater gives the bytes a whole decode does, ends where the stream does, and
/// takes input only as its output needs it.
#[test]
fn an_inflater_fed_in_pieces_matches_a_whole_decode() -> Result<()> {
    let input: Vec<u8> = (0..200_000u32).map(|i| ((i * 7) ^ (i >> 5)) as u8 % 61).collect();
    for (name, encoded, mut inflater) in [
        ("zlib", Zlib.compress(&input)?, Inflater::zlib()?),
        ("deflate", Deflate.compress(&input)?, Inflater::deflate()?),
    ] {
        let mut out = Vec::new();
        let (mut fed, mut avail) = (0usize, 0usize);
        let mut block = [0u8; 1024];
        let mut ended = false;
        while !ended {
            let mut filled = 0;
            while filled < block.len() && !ended {
                if avail == 0 {
                    let piece = encoded.len().saturating_sub(fed).min(256);
                    assert!(piece > 0, "{name}: input ran out before the stream ended");
                    fed += piece;
                    avail = piece;
                }
                let step = inflater.inflate(&encoded[fed - avail..fed], &mut block[filled..])?;
                avail -= step.consumed;
                filled += step.produced;
                ended = step.ended;
            }
            out.extend_from_slice(&block[..filled]);
        }
        assert_eq!(out, input, "{name}");
        assert_eq!(
            (inflater.total_in(), inflater.total_out()),
            (encoded.len(), input.len()),
            "{name}"
        );
        assert_eq!(avail, 0, "{name}: input left after the end");
        let after = inflater.inflate(&[1, 2, 3], &mut block)?;
        assert_eq!(
            (after.consumed, after.produced, after.ended),
            (0, 0, true),
            "{name}"
        );
    }
    // one byte of output from a stream of stored literals needs only the header and the stored block's framing
    let stored = [
        0x78, 0x01, 0x01, 0x03, 0x00, 0xFC, 0xFF, b'a', b'b', b'c', 0x02, 0x4D, 0x01, 0x27,
    ];
    let mut inflater = Inflater::zlib()?;
    let mut one = [0u8; 1];
    let step = inflater.inflate(&stored, &mut one)?;
    assert_eq!((step.consumed, step.produced, one[0]), (8, 1, b'a'));
    assert!(
        Inflater::zlib()?
            .inflate(&[0x78, 0x01, 0xFF, 0xFF], &mut one)
            .is_err()
    );
    Ok(())
}
