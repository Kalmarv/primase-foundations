//! LZO1X against two independent sources: streams laid out by hand from lzo1x_d.ch of LZO 2.10 (each covers one kind
//! of instruction and the end code), and `tests/fixtures/lzo1x`, written by python-lzo 1.15 (LZO 2.10's own
//! `lzo1x_1` and `lzo1x_999` compressors) from the synthetic inputs `gen.py` there describes. Then the refusals the
//! decoder must make.
use primase_foundations::compress::{Decompressor, Lzo1x};
use primase_foundations::hash::md5;

/// A first byte above 17 is a literal run of `byte - 17` = 4 bytes; then the end code `11 00 00` (an M4 instruction
/// whose distance part is 0).
const FIRST_LITERALS: [u8; 8] = [21, b'a', b'b', b'c', b'd', 0x11, 0x00, 0x00];
/// Instruction 1: a literal run of 1 + 3 bytes. Then M2 `0x6C`: length `(0x6C >> 5) + 1` = 4, distance
/// `1 + ((0x6C >> 2) & 7) + (0 << 3)` = 4, no trailing literals; then the end code.
const M2: [u8; 10] = [0x01, b'a', b'b', b'c', b'd', 0x6C, 0x00, 0x11, 0x00, 0x00];
/// First byte 18: one literal `x`, counted as a match's trailing literals. M3 `0x20` with length field 0: the
/// extension `00 01` makes 33 + 255 + 1 = 289; `b0 = 01, b1 = 00` is distance 1 with one trailing literal `y`. Then
/// after trailing literals, `00 00` is an M1 match: length 2, distance 1. Then the end code.
const M3_M1: [u8; 13] = [
    18, b'x', 0x20, 0x00, 0x01, 0x01, 0x00, b'y', 0x00, 0x00, 0x11, 0x00, 0x00,
];

/// Instruction 0 with the extension `00 x7, 246`: a literal run of 18 + 7 * 255 + 246 = 2049 bytes. Right after a
/// literal run, `00 00` is a 3-byte match at distance 1 + 0x800 = 2049: the run's first three bytes. Then the end code.
fn after_literal_run() -> (Vec<u8>, Vec<u8>) {
    let literals: Vec<u8> = (0..2049u32).map(|i| (i % 251) as u8).collect();
    let mut stream = vec![0x00, 0, 0, 0, 0, 0, 0, 0, 246];
    stream.extend_from_slice(&literals);
    stream.extend_from_slice(&[0x00, 0x00, 0x11, 0x00, 0x00]);
    let mut expected = literals.clone();
    expected.extend_from_slice(&literals[..3]);
    (stream, expected)
}

#[test]
fn hand_worked_streams() {
    assert_eq!(Lzo1x.decompress(&FIRST_LITERALS, 4).unwrap(), b"abcd");
    assert_eq!(Lzo1x.decompress(&M2, 8).unwrap(), b"abcdabcd");
    let mut m3 = vec![b'x'; 290];
    m3.extend_from_slice(b"yyy");
    assert_eq!(Lzo1x.decompress(&M3_M1, 293).unwrap(), m3);
    let (stream, expected) = after_literal_run();
    assert_eq!(Lzo1x.decompress(&stream, expected.len()).unwrap(), expected);
}

/// Every fixture decodes to its input: the length and MD5 `manifest.tsv` records for it.
#[test]
fn python_lzo_fixtures() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lzo1x");
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).unwrap();
    let mut checked = 0;
    for line in manifest.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [file, _input, _level, length, digest] = fields[..] else {
            panic!("manifest line {line:?}");
        };
        let packed = std::fs::read(dir.join(file)).unwrap();
        let out = Lzo1x.decompress(&packed, length.parse().unwrap()).unwrap();
        let hex: String = md5(&out).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, digest, "{file}");
        checked += 1;
    }
    assert_eq!(checked, 12);
}

#[test]
fn refusals() {
    // the wrong size either way
    assert!(Lzo1x.decompress(&FIRST_LITERALS, 3).is_err());
    assert!(Lzo1x.decompress(&FIRST_LITERALS, 5).is_err());
    assert!(Lzo1x.decompress(&M3_M1, 292).is_err());
    // every truncation, and a byte after the end code
    for n in 0..M3_M1.len() {
        assert!(Lzo1x.decompress(&M3_M1[..n], 293).is_err(), "prefix {n}");
    }
    let mut trailing = M2.to_vec();
    trailing.push(0);
    assert!(Lzo1x.decompress(&trailing, 8).is_err());
    // M2 at distance 5 after four bytes reaches before the output's start
    let before = [0x01, b'a', b'b', b'c', b'd', 0x70, 0x00, 0x11, 0x00, 0x00];
    let e = Lzo1x.decompress(&before, 8).unwrap_err().to_string();
    assert!(e.contains("input offset 5"), "{e}");
    // a claim no stream of this length can reach is refused before anything is allocated
    assert!(Lzo1x.decompress(&FIRST_LITERALS, usize::MAX).is_err());
}

/// Seeded random streams (and every fixture with one byte flipped) give a result or an error, never a panic.
#[test]
fn arbitrary_bytes_never_panic() {
    let mut s = 0x2545_F491u32;
    let mut next = || {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (s >> 16) as u8
    };
    for len in 0..64 {
        for _ in 0..200 {
            let stream: Vec<u8> = (0..len).map(|_| next()).collect();
            let _ = Lzo1x.decompress(&stream, usize::from(next()) * 4);
        }
    }
    let packed = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lzo1x/text-9.lzo1x"),
    )
    .unwrap();
    for i in 0..packed.len() {
        let mut flipped = packed.clone();
        flipped[i] ^= 0x5A;
        let _ = Lzo1x.decompress(&flipped, 19490);
    }
}
