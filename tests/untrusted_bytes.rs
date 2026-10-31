//! Hand-built hostile inputs: each must come back as an error, never a panic or an allocation sized by a claim.
use primase_foundations::compress::{
    Decompressor, Deflate, Error, Lz4, Lzo1x, Lzo2a, Zlib, ZlibUnterminated, Zstd,
};
use primase_foundations::parallel::{RowsError, Threads, rows_mut};
use primase_foundations::sampling::{Address, Levels, Transfer};

const WRAP: [Address; 2] = [Address::Wrap; 2];

/// A zero row length is an error, not a panic, on any thread count (and with no data there is nothing to refuse).
#[test]
fn rows_mut_refuses_rows_of_no_length() {
    for n in [1, 4] {
        let mut data = [0u8; 8];
        let threads = Threads::new(n).unwrap();
        assert_eq!(
            rows_mut(threads, &mut data, 0, |_, _| {}),
            Err(RowsError::NoRowLength),
            "{n} threads"
        );
    }
}

/// Levels whose public fields disagree with each other are refused by every sampling method.
#[test]
fn sampling_refuses_levels_that_do_not_hold_their_size() {
    let broken = [
        // no level at all
        Levels {
            width: 2,
            height: 2,
            levels: vec![],
        },
        // a level of 15 bytes for 2 x 2 texels
        Levels {
            width: 2,
            height: 2,
            levels: vec![vec![0; 15]],
        },
        // a base level of no width
        Levels {
            width: 0,
            height: 2,
            levels: vec![vec![]],
        },
    ];
    for l in &broken {
        let what = format!("{l:?}");
        assert!(l.texel(0, 0, 0, WRAP).is_err(), "texel {what}");
        assert!(
            l.point(0, [0.5, 0.5], WRAP, Transfer::Stored).is_err(),
            "point {what}"
        );
        assert!(
            l.bilinear(0, [0.5, 0.5], WRAP, Transfer::Stored).is_err(),
            "bilinear {what}"
        );
        assert!(l.decoded(0, Transfer::Stored).is_err(), "decoded {what}");
        assert!(
            l.trilinear([0.5, 0.5], 0.0, WRAP, Transfer::Stored).is_err(),
            "trilinear {what}"
        );
    }
    // reversing rows checks each stored level; with none stored there is nothing to reverse
    assert!(broken[0].clone().rows_reversed().is_ok());
    for l in &broken[1..] {
        assert!(l.clone().rows_reversed().is_err(), "rows_reversed {l:?}");
    }
    // a level number past the stored levels
    let one = Levels::one(2, 2, vec![0; 16]).unwrap();
    assert!(one.texel(1, 0, 0, WRAP).is_err());
    assert!(one.point(7, [0.5, 0.5], WRAP, Transfer::Stored).is_err());
    assert!(one.decoded(1, Transfer::Stored).is_err());
    // the second level of a 2 x 2 texture is short here, so only sampling it is refused
    let short = Levels {
        width: 2,
        height: 2,
        levels: vec![vec![0; 16], vec![]],
    };
    assert!(short.point(0, [0.5, 0.5], WRAP, Transfer::Stored).is_ok());
    assert!(short.trilinear([0.5, 0.5], 1.0, WRAP, Transfer::Stored).is_err());
}

/// A claimed output size no stream of this input could reach is refused before anything is allocated for it.
#[test]
fn claimed_sizes_beyond_what_the_input_can_produce_are_refused_unallocated() {
    const CLAIM: usize = 1 << 40;
    let junk = [0x10u8, 0, 0, 0, 0];
    let codecs: [(&str, &dyn Decompressor); 6] = [
        ("lz4", &Lz4),
        ("deflate", &Deflate),
        ("zlib", &Zlib),
        ("zlib-unterminated", &ZlibUnterminated),
        ("lzo1x", &Lzo1x),
        ("lzo2a", &Lzo2a),
    ];
    for (name, codec) in codecs {
        match codec.decompress(&junk, CLAIM) {
            Err(Error::Invalid { offset, detail, .. }) => {
                assert!(detail.contains(&CLAIM.to_string()), "{name}: {detail}");
                assert!(offset <= junk.len(), "{name}: offset {offset}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

/// A zstd frame header that states 1 TiB of content, with no block after it: refused as a truncated frame, the output
/// buffer growing only with what blocks produce.
#[test]
fn zstd_claiming_a_terabyte_in_a_bare_header_is_truncated_not_allocated() {
    const CLAIM: usize = 1 << 40;
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0xe0];
    frame.extend_from_slice(&(CLAIM as u64).to_le_bytes());
    match Zstd.decompress(&frame, CLAIM) {
        Err(Error::Invalid { codec: "zstd", .. }) => {}
        other => panic!("{other:?}"),
    }
}

/// Vertex refusals are matchable and carry the numbers that show what was wrong.
#[test]
fn vertex_errors_are_typed() {
    use primase_foundations::vertex::{
        Element, Error, Format, Stream, decode_stream, resolve_layout, split_triangles,
    };
    assert_eq!(Format::from_dxgi(99), Err(Error::UnsupportedFormat { id: 99 }));
    let layout = resolve_layout(&[Element {
        semantic: "POSITION".into(),
        index: 0,
        format: Format::Float3,
        slot: 0,
        offset: None,
    }])
    .unwrap();
    let stream = Stream {
        bytes: &[0; 30],
        stride: 12,
        offset: 0,
    };
    // vertices 0 and 1 fit; vertex 2 would end at byte 36
    assert!(decode_stream(&layout[0], stream, 0, 2).is_ok());
    assert_eq!(
        decode_stream(&layout[0], stream, 0, 3),
        Err(Error::StreamTooShort {
            needed: 36,
            actual: 30,
            last_vertex: 2
        })
    );
    let nan = f32::NAN.to_le_bytes().repeat(3);
    let nan = Stream {
        bytes: &nan,
        stride: 12,
        offset: 0,
    };
    assert_eq!(
        decode_stream(&layout[0], nan, 0, 1),
        Err(Error::NonFinite {
            what: "decoded element"
        })
    );
    assert_eq!(
        split_triangles(&[0, 1], 3),
        Err(Error::PartialTriangle { len: 2 })
    );
    assert_eq!(
        split_triangles(&[0, 1, 2, 0, 1, 3], 3),
        Err(Error::IndexOutOfRange {
            position: 5,
            index: 3,
            vertex_count: 3
        })
    );
}

/// A LZO2A match that reaches before the output names the offset of its operand bytes, not the end of the stream.
#[test]
fn lzo2a_match_before_the_output_names_its_operand_offset() {
    // flags 0 (literal), byte 'x'; then flags 1 0 (short match) with length bits 00, distance byte 5: 5 back, 1 written
    // flag byte 0b0000_0010: bit 0 = 0 (literal), bit 1 = 1, bit 2 = 0, bits 3-4 = 00 (length 2)
    let stream = [0b0000_0010, b'x', 5];
    match Lzo2a.decompress(&stream, 3) {
        Err(Error::Invalid {
            offset: 2, detail, ..
        }) => {
            assert!(detail.contains("expected at most 1"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}
