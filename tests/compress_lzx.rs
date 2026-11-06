//! LZX against Windows' own encoder: `tests/fixtures/lzx` holds `plain.bin` and the frames makecab packed it into, with
//! a window of 2^16 bytes (XNA's) and of 2^21 (the largest); see its gen.py. Each decodes frame by frame to exactly
//! `plain.bin`. Windows' encoder writes verbatim and aligned-offset blocks only, so uncompressed blocks are laid out by
//! hand from libmspack's lzxd.c. Then the refusals, and every mutation of a frame refused or decoded without a panic.
use primase_foundations::compress::{LZX_FRAME, LzxDecoder};

const PLAIN: &[u8] = include_bytes!("fixtures/lzx/plain.bin");
const WINDOW16: &[u8] = include_bytes!("fixtures/lzx/window16.frames");
const WINDOW21: &[u8] = include_bytes!("fixtures/lzx/window21.frames");

/// The frames of a fixture: each its compressed bytes and its decoded size.
fn frames(fixture: &[u8]) -> Vec<(&[u8], usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < fixture.len() {
        let word = |i: usize| u32::from_le_bytes(fixture[i..i + 4].try_into().expect("four bytes")) as usize;
        let (size, decoded) = (word(at), word(at + 4));
        out.push((&fixture[at + 8..at + 8 + size], decoded));
        at += 8 + size;
    }
    out
}

fn decode(bits: u32, frames: &[(&[u8], usize)]) -> Result<Vec<u8>, String> {
    let mut d = LzxDecoder::new(bits).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (i, &(input, size)) in frames.iter().enumerate() {
        out.extend_from_slice(d.frame(input, size).map_err(|e| format!("frame {i}: {e}"))?);
    }
    Ok(out)
}

#[test]
fn windows_encoder_frames_decode_to_the_plain_bytes() {
    for (bits, fixture) in [(16, WINDOW16), (21, WINDOW21)] {
        let f = frames(fixture);
        assert!(f.len() > 1, "a fixture of several frames");
        assert!(f[..f.len() - 1].iter().all(|&(_, size)| size == LZX_FRAME));
        assert_eq!(decode(bits, &f).unwrap(), PLAIN, "window 2^{bits}");
    }
}

/// Bits written most significant first into 16-bit little-endian words, as LZX reads them.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    word: u16,
    held: u32,
}

impl BitWriter {
    fn bits(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.word = self.word << 1 | (value >> i & 1) as u16;
            self.held += 1;
            if self.held == 16 {
                self.bytes.extend_from_slice(&self.word.to_le_bytes());
                (self.word, self.held) = (0, 0);
            }
        }
    }
    /// The words so far, the last padded with zeros: what an uncompressed block's header aligns to.
    fn aligned(mut self) -> Vec<u8> {
        if self.held > 0 {
            self.bits(0, 16 - self.held);
        }
        self.bytes
    }
}

/// An uncompressed block's header: its type (3) and length, then the bits to the next word and the three repeated
/// offsets (1, 1, 1).
fn stored_header(w: &mut BitWriter, length: u32) {
    w.bits(3, 3);
    w.bits(length >> 8, 16);
    w.bits(length & 0xFF, 8);
}

#[test]
fn uncompressed_blocks_hand_laid_out() {
    // no translation; a stored block of 5 bytes, its pad byte, then on a bit stream started afresh one of 8
    let mut w = BitWriter::default();
    w.bits(0, 1);
    stored_header(&mut w, 5);
    let mut frame = w.aligned();
    frame.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    frame.extend_from_slice(b"hello\0");
    let mut w = BitWriter::default();
    stored_header(&mut w, 8);
    frame.extend(w.aligned());
    frame.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    frame.extend_from_slice(b" world!!");
    let mut d = LzxDecoder::new(16).unwrap();
    assert_eq!(d.frame(&frame, 13).unwrap(), b"hello world!!");
    // without the pad byte the second block's header is read from the stored bytes' end: refused
    let mut unpadded = frame.clone();
    unpadded.remove(4 + 12 + 5);
    assert!(LzxDecoder::new(16).unwrap().frame(&unpadded, 13).is_err());
    // a repeated offset of 0 is refused
    let mut zero = frame.clone();
    zero[4] = 0;
    let e = LzxDecoder::new(16)
        .unwrap()
        .frame(&zero, 13)
        .unwrap_err()
        .to_string();
    assert!(e.contains("repeated offset of 0"), "{e}");
}

#[test]
fn refusals() {
    let f = frames(WINDOW16);
    // window sizes outside LZX's
    assert!(LzxDecoder::new(14).is_err());
    assert!(LzxDecoder::new(22).is_err());
    // a frame size outside 1 to 32 KiB
    let mut d = LzxDecoder::new(16).unwrap();
    assert!(d.frame(f[0].0, 0).is_err());
    assert!(d.frame(f[0].0, LZX_FRAME + 1).is_err());
    // a frame cut short (even by one byte: a word's second byte is read first), and one with a word after it
    for cut in [1, 2, f[0].0.len() / 2] {
        let mut d = LzxDecoder::new(16).unwrap();
        let e = d
            .frame(&f[0].0[..f[0].0.len() - cut], f[0].1)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("lzx at input offset"), "{e}");
    }
    let mut longer = f[0].0.to_vec();
    longer.extend_from_slice(&[0, 0]);
    let e = LzxDecoder::new(16)
        .unwrap()
        .frame(&longer, f[0].1)
        .unwrap_err()
        .to_string();
    assert!(e.contains("bytes after the frame"), "{e}");
    // a frame asked for more bytes than it holds runs out of input
    let mut d = LzxDecoder::new(16).unwrap();
    assert!(d.frame(f[0].0, f[0].1 - 1).is_err());
    // block type 0: the first bit (no translation), then 000 and a length
    let e = LzxDecoder::new(16)
        .unwrap()
        .frame(&[0x00, 0x00, 0x00, 0x10], 16)
        .unwrap_err()
        .to_string();
    assert!(e.contains("block type 0"), "{e}");
}

#[test]
fn mutated_frames_are_refused_or_decoded_never_a_panic() {
    let f = frames(WINDOW16);
    let (first, size) = f[0];
    // every byte of the first 512, and every 61st after, with three bit flips each
    for at in (0..first.len()).filter(|&i| i < 512 || i % 61 == 0) {
        for flip in [0x01u8, 0x10, 0x80] {
            let mut m = first.to_vec();
            m[at] ^= flip;
            let mut d = LzxDecoder::new(16).unwrap();
            if let Ok(out) = d.frame(&m, size) {
                assert_eq!(out.len(), size);
            }
        }
    }
}
