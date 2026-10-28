//! The framing of an Oodle stream, walked without decoding it.
//!
//! An Oodle stream is a run of *blocks*, one per 262,144 decoded bytes (the last holds what is left). A block starts
//! with a two-byte header that names its decoder; the quanta that follow are framed by that decoder type: one 3-byte
//! big-endian quantum header per block for Kraken, Mermaid and Leviathan, sixteen 2-byte ones (a quantum per 16,384
//! decoded bytes) for LZB16, LZNA and BitKnit. [`walk`] reads those headers, steps over each quantum's body by the size
//! its header states, and checks that the walk ends exactly at the end of the payload and the decoded size: a stream
//! whose last block stops short, runs past the payload, or has bytes left over is refused, naming where. The one-shot
//! library call ([`Oodle::decompress`](crate::compress::oodle::Oodle)) reports neither how many input bytes it used
//! nor that some were left over, so this is the check for an exactly framed payload.
//!
//! The framing was read from the payloads of both FromSoftware games the crate's readers serve: every `KRAK` payload of
//! Sekiro and Elden Ring, binder members included (see [`Framing`]), walks to exactly its last byte. Only decoder types
//! 6 (Kraken) and 2 (LZB16) occur there; the other types' framing is oozextract's and is untested against any stream.
//! No payload sets the checksum bit, and the layout of a quantum with checksums is not established, so a block that sets
//! it is refused rather than walked.
use crate::compress::{Result, invalid};

/// Decoded bytes per block, and so per block header.
pub const BLOCK_SIZE: usize = 0x40000;
/// Decoded bytes per quantum of the small-quantum decoders (LZB16, LZNA, BitKnit).
pub const SMALL_QUANTUM: usize = 0x4000;

const CODEC: &str = "oodle framing";

/// The decoder a block header names (oozextract's list of the types it reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DecoderType {
    /// 2: LZB16, 16 quanta per block. The one-quantum streams of Sekiro and Elden Ring's small files.
    Lzb16,
    /// 5: LZNA, 16 quanta per block.
    Lzna,
    /// 6: Kraken, one quantum per block. Almost every payload of both games.
    Kraken,
    /// 10: Mermaid, one quantum per block.
    Mermaid,
    /// 11: BitKnit, 16 quanta per block.
    Bitknit,
    /// 12: Leviathan, one quantum per block.
    Leviathan,
}

impl DecoderType {
    /// The decoder numbered `id` in a block header's second byte (bits 0-6), if it is one the walker knows.
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            2 => Self::Lzb16,
            5 => Self::Lzna,
            6 => Self::Kraken,
            10 => Self::Mermaid,
            11 => Self::Bitknit,
            12 => Self::Leviathan,
            _ => return None,
        })
    }

    /// The number a block header stores.
    pub fn id(self) -> u8 {
        match self {
            Self::Lzb16 => 2,
            Self::Lzna => 5,
            Self::Kraken => 6,
            Self::Mermaid => 10,
            Self::Bitknit => 11,
            Self::Leviathan => 12,
        }
    }

    /// Decoded bytes one quantum of this decoder covers: [`SMALL_QUANTUM`] or [`BLOCK_SIZE`].
    pub fn quantum_size(self) -> usize {
        match self {
            Self::Lzb16 | Self::Lzna | Self::Bitknit => SMALL_QUANTUM,
            Self::Kraken | Self::Mermaid | Self::Leviathan => BLOCK_SIZE,
        }
    }
}

/// How many quanta of each kind a block holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuantumCounts {
    /// Quanta with a compressed body of the size their header states.
    pub compressed: u32,
    /// Quanta that repeat one fill byte over the whole quantum.
    pub memset: u32,
    /// Quanta that copy the whole quantum from earlier output (small-quantum decoders only).
    pub whole_match: u32,
    /// Quanta stored as raw bytes after a small-quantum header (small-quantum decoders only).
    pub stored: u32,
}

/// One block of a walked stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// The two header bytes as stored.
    pub header: [u8; 2],
    /// Where the header starts in the payload.
    pub offset: usize,
    /// Decoded bytes the block covers.
    pub decoded: usize,
    /// The decoder the header names.
    pub decoder: DecoderType,
    /// The header's restart bit (byte 0, bit 7): the decoder starts afresh here.
    pub restart: bool,
    /// The header's stored bit (byte 0, bit 6): the block is raw bytes with no quantum headers.
    pub stored: bool,
    /// The quanta of the block (all zero for a stored block).
    pub quanta: QuantumCounts,
}

/// What [`walk`] found in a stream that is exactly framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framing {
    /// Every block, in stream order.
    pub blocks: Vec<Block>,
}

impl Framing {
    /// Blocks in the stream.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// The decoder types the stream's blocks name, each once, in numeric order.
    pub fn decoders(&self) -> Vec<DecoderType> {
        let mut found: Vec<DecoderType> = self.blocks.iter().map(|b| b.decoder).collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Blocks whose header has the restart bit set.
    pub fn restart_bits(&self) -> usize {
        self.blocks.iter().filter(|b| b.restart).count()
    }

    /// Blocks stored as raw bytes.
    pub fn stored_blocks(&self) -> usize {
        self.blocks.iter().filter(|b| b.stored).count()
    }

    /// The quanta of every block together.
    pub fn quanta(&self) -> QuantumCounts {
        self.blocks
            .iter()
            .fold(QuantumCounts::default(), |all, b| QuantumCounts {
                compressed: all.compressed.saturating_add(b.quanta.compressed),
                memset: all.memset.saturating_add(b.quanta.memset),
                whole_match: all.whole_match.saturating_add(b.quanta.whole_match),
                stored: all.stored.saturating_add(b.quanta.stored),
            })
    }
}

/// A cursor over the payload that refuses to read past its end and says where.
struct Reader<'a> {
    payload: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    /// The end of the next `n` bytes, if the payload holds them.
    fn end_of(&self, n: usize) -> Option<usize> {
        self.pos.checked_add(n).filter(|&end| end <= self.payload.len())
    }

    fn bytes<const N: usize>(&mut self, what: &str) -> Result<[u8; N]> {
        let got = self
            .end_of(N)
            .and_then(|end| Some((end, self.payload.get(self.pos..end)?)))
            .and_then(|(end, s)| Some((end, <[u8; N]>::try_from(s).ok()?)));
        let Some((end, got)) = got else {
            return Err(invalid(
                CODEC,
                self.pos,
                format!(
                    "{what} needs {N} bytes at {}, but the payload ends at {}",
                    self.pos,
                    self.payload.len()
                ),
            ));
        };
        self.pos = end;
        Ok(got)
    }

    /// Steps over `n` body bytes.
    fn skip(&mut self, n: usize, what: &str) -> Result<()> {
        match self.end_of(n) {
            Some(end) => {
                self.pos = end;
                Ok(())
            }
            None => Err(invalid(
                CODEC,
                self.pos,
                format!(
                    "{what} of {n} bytes at {} runs past the payload, which ends at {}",
                    self.pos,
                    self.payload.len()
                ),
            )),
        }
    }
}

/// Walks the block and quantum headers of an Oodle `payload` that decodes to `decoded_size` bytes, without decoding.
///
/// Refuses, naming the block and the payload offset: a block header that is not `xC xx` with bits 4-5 clear, an unknown
/// decoder type, a block that sets the checksum bit, a quantum header that is not one of the defined kinds, a header or
/// body that runs past the payload, and bytes left after the last quantum. An empty stream decodes to zero bytes and no
/// other size. The work is proportional to the payload (a block takes at least two of its bytes), never to `decoded_size`.
pub fn walk(payload: &[u8], decoded_size: usize) -> Result<Framing> {
    let mut blocks = Vec::new();
    let mut r = Reader { payload, pos: 0 };
    let mut done = 0usize;
    while done < decoded_size {
        let index = blocks.len();
        let offset = r.pos;
        let header = r.bytes::<2>(&format!("block {index}'s header"))?;
        let [b0, b1] = header;
        if b0 & 0xF != 0xC || (b0 >> 4) & 3 != 0 {
            return Err(invalid(
                CODEC,
                offset,
                format!("block {index} header {b0:02X} {b1:02X} is not a block header"),
            ));
        }
        if b1 & 0x80 != 0 {
            return Err(invalid(
                CODEC,
                offset,
                format!(
                    "block {index} header {b0:02X} {b1:02X} sets the checksum bit, whose quantum layout is not supported"
                ),
            ));
        }
        let Some(decoder) = DecoderType::from_id(b1 & 0x7F) else {
            return Err(invalid(
                CODEC,
                offset,
                format!(
                    "block {index} header {b0:02X} {b1:02X} names unknown decoder type {}",
                    b1 & 0x7F
                ),
            ));
        };
        let stored = b0 & 0x40 != 0;
        let covers = decoded_size.saturating_sub(done).min(BLOCK_SIZE);
        let mut quanta = QuantumCounts::default();
        if stored {
            r.skip(covers, &format!("stored block {index}"))?;
        } else {
            let step = decoder.quantum_size();
            let mut left = covers;
            while left > 0 {
                let here = left.min(step);
                walk_quantum(&mut r, decoder, here, index, &mut quanta)?;
                left = left.saturating_sub(here);
            }
        }
        blocks.push(Block {
            header,
            offset,
            decoded: covers,
            decoder,
            restart: b0 & 0x80 != 0,
            stored,
            quanta,
        });
        done = done.saturating_add(covers);
    }
    if r.pos != payload.len() {
        return Err(invalid(
            CODEC,
            r.pos,
            format!(
                "{} trailing bytes after the last quantum, which ends the {decoded_size} decoded bytes at {}",
                payload.len().saturating_sub(r.pos),
                r.pos
            ),
        ));
    }
    Ok(Framing { blocks })
}

/// One quantum of `here` decoded bytes: its header and body.
fn walk_quantum(
    r: &mut Reader<'_>,
    decoder: DecoderType,
    here: usize,
    block: usize,
    counts: &mut QuantumCounts,
) -> Result<()> {
    let at = r.pos;
    if decoder.quantum_size() == BLOCK_SIZE {
        let [a, b, c] = r.bytes::<3>("quantum header")?;
        let v = usize::from(a) << 16 | usize::from(b) << 8 | usize::from(c);
        let size = v & 0x3FFFF;
        if size != 0x3FFFF {
            r.skip(size.saturating_add(1), &format!("quantum body in block {block}"))?;
            counts.compressed = counts.compressed.saturating_add(1);
        } else if v >> 18 == 1 {
            r.skip(1, &format!("memset fill byte in block {block}"))?;
            counts.memset = counts.memset.saturating_add(1);
        } else {
            return Err(invalid(
                CODEC,
                at,
                format!(
                    "block {block} has quantum header {v:#08X}, which is not a compressed size or a memset"
                ),
            ));
        }
        return Ok(());
    }
    let [a, b] = r.bytes::<2>("quantum header")?;
    let v = usize::from(a) << 8 | usize::from(b);
    let size = v & 0x3FFF;
    if size != 0x3FFF {
        r.skip(size.saturating_add(1), &format!("quantum body in block {block}"))?;
        counts.compressed = counts.compressed.saturating_add(1);
        return Ok(());
    }
    match v >> 14 {
        0 => {
            // a whole-match distance: two bytes, and for a value under 0x8000 a run of continuation bytes up to and
            // including the first with bit 7 set
            let [d0, d1] = r.bytes::<2>("whole-match distance")?;
            if usize::from(d0) << 8 | usize::from(d1) < 0x8000 {
                loop {
                    let [more] = r.bytes::<1>("whole-match distance continuation")?;
                    if more & 0x80 != 0 {
                        break;
                    }
                }
            }
            counts.whole_match = counts.whole_match.saturating_add(1);
        }
        1 => {
            r.skip(1, &format!("memset fill byte in block {block}"))?;
            counts.memset = counts.memset.saturating_add(1);
        }
        2 => {
            r.skip(here, &format!("stored quantum in block {block}"))?;
            counts.stored = counts.stored.saturating_add(1);
        }
        _ => {
            return Err(invalid(
                CODEC,
                at,
                format!("block {block} has quantum header {v:#06X}, which is not a defined escape"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block header and Kraken quantum of `body` bytes (the body's content is irrelevant to framing).
    fn kraken_block(flags: u8, body: usize) -> Vec<u8> {
        let mut v = vec![flags, 0x06];
        let size = body - 1;
        v.extend([(size >> 16) as u8, (size >> 8) as u8, size as u8]);
        v.extend(std::iter::repeat_n(0xAB, body));
        v
    }

    fn lzb16_block(flags: u8, bodies: &[usize]) -> Vec<u8> {
        let mut v = vec![flags, 0x02];
        for &body in bodies {
            let size = body - 1;
            v.extend([(size >> 8) as u8, size as u8]);
            v.extend(std::iter::repeat_n(0xCD, body));
        }
        v
    }

    fn message(r: Result<Framing>) -> String {
        r.expect_err("the walk should refuse").to_string()
    }

    #[test]
    fn empty_stream_is_exactly_empty() {
        let f = walk(&[], 0).unwrap();
        assert_eq!(f.block_count(), 0);
        assert!(f.decoders().is_empty());
        assert!(message(walk(&[], 1)).contains("block 0's header needs 2 bytes"));
        assert!(message(walk(&[0x8C], 0)).contains("1 trailing bytes"));
    }

    #[test]
    fn one_kraken_block() {
        let s = kraken_block(0x8C, 100);
        let f = walk(&s, 5000).unwrap();
        assert_eq!(f.block_count(), 1);
        assert_eq!(f.decoders(), [DecoderType::Kraken]);
        assert_eq!(f.restart_bits(), 1);
        assert_eq!(f.stored_blocks(), 0);
        let b = f.blocks[0];
        assert_eq!((b.header, b.offset, b.decoded), ([0x8C, 0x06], 0, 5000));
        assert_eq!(f.quanta().compressed, 1);
    }

    #[test]
    fn blocks_split_every_quarter_mebibyte() {
        let mut s = kraken_block(0x8C, 10);
        s.extend(kraken_block(0x8C, 20));
        s.extend(kraken_block(0x0C, 30));
        let f = walk(&s, 2 * BLOCK_SIZE + 1).unwrap();
        assert_eq!(f.block_count(), 3);
        assert_eq!(f.restart_bits(), 2);
        assert_eq!(
            f.blocks.iter().map(|b| (b.offset, b.decoded)).collect::<Vec<_>>(),
            [(0, BLOCK_SIZE), (15, BLOCK_SIZE), (40, 1)]
        );
        // the same bytes, but a decoded size that needs only two blocks leave the third's bytes over
        assert!(message(walk(&s, 2 * BLOCK_SIZE)).contains("trailing bytes"));
        // and one that needs a fourth runs out
        assert!(message(walk(&s, 3 * BLOCK_SIZE + 1)).contains("block 3's header needs 2 bytes"));
    }

    #[test]
    fn memset_quantum_has_one_fill_byte() {
        let s = [0x8C, 0x06, 0x07, 0xFF, 0xFF, 0x7A];
        let f = walk(&s, BLOCK_SIZE).unwrap();
        assert_eq!(
            f.quanta(),
            QuantumCounts {
                memset: 1,
                ..Default::default()
            }
        );
        // 0x3FFFF with another flag value is not a memset
        assert!(
            message(walk(&[0x8C, 0x06, 0x03, 0xFF, 0xFF, 0x7A], BLOCK_SIZE))
                .contains("not a compressed size or a memset")
        );
        // a missing fill byte
        assert!(message(walk(&s[..5], BLOCK_SIZE)).contains("memset fill byte"));
    }

    #[test]
    fn stored_block_is_raw_bytes_without_a_quantum_header() {
        let mut s = vec![0xCC, 0x06];
        s.extend(std::iter::repeat_n(1, 1000));
        let f = walk(&s, 1000).unwrap();
        assert_eq!(f.stored_blocks(), 1);
        assert!(f.blocks[0].stored && f.blocks[0].restart);
        assert_eq!(f.quanta(), QuantumCounts::default());
        // a stored full block followed by a Kraken block
        let mut two = vec![0xCC, 0x06];
        two.extend(std::iter::repeat_n(1, BLOCK_SIZE));
        two.extend(kraken_block(0x8C, 7));
        assert_eq!(walk(&two, BLOCK_SIZE + 9).unwrap().stored_blocks(), 1);
        assert!(message(walk(&s[..s.len() - 1], 1000)).contains("stored block 0 of 1000 bytes"));
    }

    #[test]
    fn lzb16_block_has_a_quantum_per_sixteen_kib() {
        let s = lzb16_block(0x8C, &[10, 20, 30]);
        let f = walk(&s, 2 * SMALL_QUANTUM + 5).unwrap();
        assert_eq!(f.decoders(), [DecoderType::Lzb16]);
        assert_eq!(f.quanta().compressed, 3);
        // a decoded size that needs two quanta leaves the third's bytes over; one that needs four runs out
        assert!(message(walk(&s, 2 * SMALL_QUANTUM)).contains("trailing bytes"));
        assert!(message(walk(&s, 3 * SMALL_QUANTUM + 1)).contains("quantum header needs 2 bytes"));
    }

    #[test]
    fn lzb16_escapes() {
        // memset: 0x7FFF, a fill byte
        let f = walk(&[0x8C, 0x02, 0x7F, 0xFF, 0x00], 100).unwrap();
        assert_eq!(f.quanta().memset, 1);
        // stored quantum: 0xBFFF, then the quantum's bytes (here 100)
        let mut s = vec![0x8C, 0x02, 0xBF, 0xFF];
        s.extend(std::iter::repeat_n(9, 100));
        assert_eq!(walk(&s, 100).unwrap().quanta().stored, 1);
        // whole match, short form: distance high bit set, two bytes
        assert_eq!(
            walk(&[0x8C, 0x02, 0x3F, 0xFF, 0x80, 0x10], 100)
                .unwrap()
                .quanta()
                .whole_match,
            1
        );
        // whole match, long form: two bytes under 0x8000, then continuation bytes up to one with bit 7 set
        let f = walk(&[0x8C, 0x02, 0x3F, 0xFF, 0x00, 0x10, 0x01, 0x02, 0x83], 100).unwrap();
        assert_eq!(f.quanta().whole_match, 1);
        assert!(
            message(walk(&[0x8C, 0x02, 0x3F, 0xFF, 0x00, 0x10, 0x01, 0x02], 100)).contains("continuation")
        );
        // 0xFFFF is no escape
        assert!(message(walk(&[0x8C, 0x02, 0xFF, 0xFF], 100)).contains("not a defined escape"));
    }

    #[test]
    fn block_headers_are_validated() {
        let ok = kraken_block(0x8C, 4);
        for bad in [0x8Du8, 0x9C, 0xAC, 0x8B, 0x00] {
            let mut s = ok.clone();
            s[0] = bad;
            assert!(
                message(walk(&s, 100)).contains("is not a block header"),
                "{bad:02X}"
            );
        }
        let mut s = ok.clone();
        s[1] = 0x07;
        assert!(message(walk(&s, 100)).contains("unknown decoder type 7"));
        s[1] = 0x86;
        assert!(message(walk(&s, 100)).contains("checksum bit"));
        // later blocks are checked too, and the error says which
        let mut two = kraken_block(0x8C, 4);
        two.extend([0x3C, 0x06, 0x00, 0x00, 0x00, 0x00]);
        assert!(message(walk(&two, BLOCK_SIZE + 1)).contains("block 1 header 3C 06"));
    }

    #[test]
    fn exact_consumption_is_required() {
        let s = kraken_block(0x8C, 100);
        walk(&s, 5000).unwrap();
        // one byte short: the quantum body runs past the payload
        let err = message(walk(&s[..s.len() - 1], 5000));
        assert!(err.contains("quantum body in block 0 of 100 bytes"), "{err}");
        // one byte over
        let mut over = s.clone();
        over.push(0);
        let err = message(walk(&over, 5000));
        assert!(err.contains("1 trailing bytes"), "{err}");
        // a header cut off in the middle
        assert!(message(walk(&s[..3], 5000)).contains("quantum header needs 3 bytes"));
        assert!(message(walk(&s[..1], 5000)).contains("block 0's header needs 2 bytes"));
    }

    #[test]
    fn work_follows_the_payload_not_the_claimed_size() {
        // a claim of the whole address space against an empty payload is refused at the first header
        assert!(message(walk(&[], usize::MAX)).contains("block 0's header needs 2 bytes"));
        // and against two one-byte memset blocks, at the third
        let s = [
            0x8C, 0x06, 0x07, 0xFF, 0xFF, 0x11, 0x8C, 0x06, 0x07, 0xFF, 0xFF, 0x11,
        ];
        assert!(message(walk(&s, usize::MAX)).contains("block 2's header needs 2 bytes"));
    }

    #[test]
    fn every_cut_of_a_valid_stream_is_refused_and_noise_never_panics() {
        let mut s = kraken_block(0x8C, 40);
        s.extend(kraken_block(0xCC, 1).into_iter().take(2));
        s.extend(std::iter::repeat_n(5, BLOCK_SIZE));
        s.extend(kraken_block(0x8C, 9));
        let decoded = 2 * BLOCK_SIZE + 77;
        walk(&s, decoded).unwrap();
        for cut in 0..s.len() {
            assert!(walk(&s[..cut], decoded).is_err(), "cut at {cut}");
        }
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let n = (next() % 64) as usize;
            let mut noise: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            // headers that look right more often than chance would give them
            if let Some(b) = noise.first_mut() {
                *b = 0x8C;
            }
            if let Some(b) = noise.get_mut(1) {
                *b = [2, 6, 10, 11, 5, 12][(next() % 6) as usize];
            }
            let _ = walk(&noise, (next() % (3 * BLOCK_SIZE as u64)) as usize);
        }
    }

    #[test]
    fn decoder_ids_round_trip() {
        for id in 0..=0x7F {
            if let Some(t) = DecoderType::from_id(id) {
                assert_eq!(t.id(), id);
            }
        }
        assert_eq!(DecoderType::from_id(6), Some(DecoderType::Kraken));
        assert_eq!(DecoderType::from_id(3), None);
    }
}
