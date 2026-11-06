//! LZX, the LZ77 and Huffman codec of Microsoft's cabinet files, which XNA 4.0 compresses its content (`.xnb`) with,
//! decoded one frame at a time ([`LzxDecoder`]).
//!
//! The stream (libmspack's `lzxd.c` and MonoGame's `LzxDecoder` read it): a window of 2^15 to 2^21 bytes, filled in
//! frames of 32 KiB (the last may be shorter). Bits are read from 16-bit little-endian words, most significant bit
//! first. The first frame starts with one bit, set when x86 `E8` call translation was applied, then (if set) the
//! 32-bit size the translation used. Then blocks, each a 3-bit type and a 24-bit decoded length:
//! - **verbatim** (1): the main tree (256 literals, then eight match lengths per position slot) and the length tree,
//!   each sent as deltas against the previous block's code lengths through a 20-symbol pretree; then literals and
//!   matches.
//! - **aligned offset** (2): an aligned-offset tree of 8 three-bit lengths that codes the low three bits of long match
//!   offsets, then as verbatim.
//! - **uncompressed** (3): the bit stream skips to its next 16-bit word (1 to 16 bits), then the three repeated
//!   offsets as 32-bit values and the bytes as stored, padded to an even length.
//!
//! A match's position slot 0, 1 or 2 repeats one of the last three offsets; slot 3 and above are a base plus extra
//! bits. A match may run past the end of its frame: the bytes it makes there are the next frame's first. Once a frame
//! is decoded, if translation is on, a block has used the `E8` literal or stored bytes, and the frame is among the
//! first 32,768 and longer than 10 bytes, every `E8` byte before the frame's last 10 is followed by an absolute 32-bit
//! offset, turned back into the relative one it was.
//!
//! Strict, like the crate's other codecs: a frame must make exactly its stated size from its input, a bit read past
//! the input is refused, and so is input left after the frame beyond the padding to its last 16-bit word. A set of code
//! lengths that is neither a complete code nor empty, a symbol of an empty tree, and a match before the stream's start
//! or across the window's end are refused. Memory is the window, the trees and one frame: nothing is sized by the
//! input.

use crate::compress::{Error, Result, invalid};

const CODEC: &str = "lzx";

/// The largest frame: LZX decodes in frames of this size, the last of a stream shorter.
pub const LZX_FRAME: usize = 32 * 1024;

const LITERALS: usize = 256;
const MIN_MATCH: usize = 2;
const PRIMARY_LENGTHS: usize = 7;
const SECONDARY_LENGTHS: usize = 249;
const PRETREE_SYMBOLS: usize = 20;
const ALIGNED_SYMBOLS: usize = 8;
const MAX_POSITION_SLOTS: usize = 50;
const MAIN_MAX: usize = LITERALS + MAX_POSITION_SLOTS * 8;
/// Room after the last code length that a pretree run may write into, as libmspack's tables have.
const LENGTH_SLACK: usize = 64;
/// The longest code a tree may have.
const MAX_CODE: u32 = 16;
/// Translation is applied to the first this many frames only.
const E8_FRAMES: u64 = 32_768;
/// The literal whose use in a block starts translation.
const E8: usize = 0xE8;

const VERBATIM: u32 = 1;
const ALIGNED: u32 = 2;
const UNCOMPRESSED: u32 = 3;

/// Extra bits of each position slot: 0 for the first four, then 1, 1, 2, 2, ... up to 17 (libmspack's table).
const EXTRA_BITS: [u32; MAX_POSITION_SLOTS] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14,
    15, 15, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
];

/// The first offset of each position slot, plus 2: slot `i` spans `1 << EXTRA_BITS[i]` offsets.
const POSITION_BASE: [u32; MAX_POSITION_SLOTS] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072,
    4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608, 262144, 393216,
    524288, 655360, 786432, 917504, 1048576, 1179648, 1310720, 1441792, 1572864, 1703936, 1835008, 1966080,
];

/// The bits of one frame's input: 16-bit little-endian words, most significant bit first (a word's second byte is
/// read before its first). It reads zeros past the input (a decoder peeks ahead of what it uses), but consuming a bit
/// of a word the input does not hold whole is refused.
struct Bits<'a> {
    input: &'a [u8],
    /// Where the words start: 0, or the byte after bytes read as stored.
    base: usize,
    /// The next byte to load.
    next: usize,
    buffer: u32,
    held: u32,
    /// Bits consumed since `base`.
    used: u64,
}

impl<'a> Bits<'a> {
    fn new(input: &'a [u8]) -> Self {
        Bits {
            input,
            base: 0,
            next: 0,
            buffer: 0,
            held: 0,
            used: 0,
        }
    }

    /// The byte of the next bit.
    fn at(&self) -> usize {
        self.base
            .saturating_add(usize::try_from(self.used / 8).unwrap_or(usize::MAX))
    }

    /// The byte after the last word a bit was consumed from.
    fn words_end(&self) -> usize {
        let words = usize::try_from(self.used.div_ceil(16)).unwrap_or(usize::MAX);
        self.base.saturating_add(words.saturating_mul(2))
    }

    /// Hold more than 16 bits, loading words (zeros past the input).
    fn fill(&mut self) {
        while self.held <= 16 {
            let byte = |i: usize| self.input.get(i).copied().unwrap_or(0);
            let word = u32::from(u16::from_le_bytes([
                byte(self.next),
                byte(self.next.saturating_add(1)),
            ]));
            self.next = self.next.saturating_add(2);
            self.buffer |= word.wrapping_shl(16u32.saturating_sub(self.held));
            self.held = self.held.saturating_add(16);
        }
    }

    /// The next `n` (1 to 16) bits, left in place.
    fn peek(&mut self, n: u32) -> u32 {
        self.fill();
        self.buffer.checked_shr(32u32.saturating_sub(n)).unwrap_or(0)
    }

    fn consume(&mut self, n: u32) -> Result<()> {
        self.buffer = self.buffer.checked_shl(n).unwrap_or(0);
        self.held = self.held.saturating_sub(n);
        self.used = self.used.saturating_add(u64::from(n));
        if self.words_end() > self.input.len() {
            return Err(invalid(
                CODEC,
                self.input.len(),
                "the frame's input ends before its output does",
            ));
        }
        Ok(())
    }

    /// The next `n` (0 to 16) bits as a number.
    fn read(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.peek(n);
        self.consume(n)?;
        Ok(v)
    }

    /// Skip to the next 16-bit word: 1 to 16 bits, as an uncompressed block's header has it (the next bit is then on
    /// a byte boundary).
    fn align(&mut self) -> Result<()> {
        let into = u32::try_from(self.used % 16).unwrap_or(0);
        self.consume(16u32.saturating_sub(into))
    }

    /// `n` bytes as stored, at the next bit (which is on a byte boundary); the bit reader restarts after them.
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let at = self.at();
        let bytes = at
            .checked_add(n)
            .and_then(|end| self.input.get(at..end))
            .ok_or_else(|| {
                invalid(
                    CODEC,
                    at,
                    format!(
                        "{n} stored bytes wanted, {} left",
                        self.input.len().saturating_sub(at)
                    ),
                )
            })?;
        self.restart(at.saturating_add(n));
        Ok(bytes)
    }

    /// Restart the words at byte `at`, after bytes read as stored.
    fn restart(&mut self, at: usize) {
        self.base = at;
        self.next = at;
        self.buffer = 0;
        self.held = 0;
        self.used = 0;
    }

    /// Bytes of the input after the last word a bit was consumed from (or after the last stored byte).
    fn left(&self) -> usize {
        self.input.len().saturating_sub(self.words_end())
    }
}

/// A canonical Huffman code: a table for codes of up to `table_bits` bits, and per length the first code and its
/// first symbol for longer ones.
#[derive(Clone)]
struct Tree {
    name: &'static str,
    /// Code lengths, with the slack a pretree run may write past the last symbol.
    lengths: Vec<u8>,
    symbols: usize,
    table_bits: u32,
    /// `(symbol, length)` for each `table_bits`-bit prefix that begins a code of at most `table_bits` bits.
    table: Vec<(u16, u8)>,
    /// The symbols in code order.
    sorted: Vec<u16>,
    /// Per length: how many codes, the first code, and its index in `sorted`.
    count: [u32; MAX_CODE as usize + 1],
    first: [u32; MAX_CODE as usize + 1],
    index: [u32; MAX_CODE as usize + 1],
}

impl Tree {
    fn new(name: &'static str, symbols: usize, table_bits: u32) -> Self {
        Tree {
            name,
            lengths: vec![0; symbols.saturating_add(LENGTH_SLACK)],
            symbols,
            table_bits,
            table: Vec::new(),
            sorted: Vec::new(),
            count: [0; MAX_CODE as usize + 1],
            first: [0; MAX_CODE as usize + 1],
            index: [0; MAX_CODE as usize + 1],
        }
    }

    fn set_length(&mut self, symbol: usize, length: u32, at: usize) -> Result<()> {
        let slot = self.lengths.get_mut(symbol).ok_or_else(|| {
            invalid(
                CODEC,
                at,
                format!("{} tree: a code length past its symbols", self.name),
            )
        })?;
        *slot = u8::try_from(length).map_err(|_| invalid(CODEC, at, "a code length past 255"))?;
        Ok(())
    }

    /// Build the decoding tables from the first `symbols` lengths, which must make a complete code or none.
    fn build(&mut self, at: usize) -> Result<()> {
        let mut count = [0u32; MAX_CODE as usize + 1];
        for &l in self.lengths.iter().take(self.symbols) {
            let c = count
                .get_mut(usize::from(l))
                .ok_or_else(|| invalid(CODEC, at, format!("{} tree: a code of {l} bits", self.name)))?;
            *c = c.saturating_add(1);
        }
        count[0] = 0;
        self.table.clear();
        self.sorted.clear();
        self.count = count;
        if count.iter().all(|&c| c == 0) {
            return Ok(());
        }
        // the codes must fill the code space exactly (Kraft)
        let mut space = 0u64;
        for (bits, &c) in (0u32..).zip(count.iter()).skip(1) {
            space = space.saturating_add(
                u64::from(c)
                    .checked_shl(MAX_CODE.saturating_sub(bits))
                    .unwrap_or(u64::MAX),
            );
        }
        if space != 1 << MAX_CODE {
            let how = if space > 1 << MAX_CODE {
                "overfill"
            } else {
                "do not fill"
            };
            return Err(invalid(
                CODEC,
                at,
                format!("{} tree: its code lengths {how} the code space", self.name),
            ));
        }
        let (mut code, mut index) = (0u32, 0u32);
        for l in 1..=MAX_CODE as usize {
            let before = count.get(l.saturating_sub(1)).copied().unwrap_or(0);
            code = code.saturating_add(before).saturating_mul(2);
            if let (Some(f), Some(i)) = (self.first.get_mut(l), self.index.get_mut(l)) {
                *f = code;
                *i = index;
            }
            index = index.saturating_add(count.get(l).copied().unwrap_or(0));
        }
        for l in 1..=u8::try_from(MAX_CODE).unwrap_or(u8::MAX) {
            for (s, &sl) in (0u16..).zip(self.lengths.iter().take(self.symbols)) {
                if sl == l {
                    self.sorted.push(s);
                }
            }
        }
        // each code of at most `table_bits` bits fills the table entries its bits begin
        self.table = vec![(0, 0); 1 << self.table_bits];
        for l in 1..=self.table_bits {
            let li = l as usize;
            let (n, first, index) = (
                at_length(&self.count, li),
                at_length(&self.first, li),
                at_length(&self.index, li),
            );
            let shift = self.table_bits.saturating_sub(l);
            for k in 0..n {
                let Some(&symbol) = self.sorted.get(index.saturating_add(k) as usize) else {
                    continue;
                };
                let start = first.saturating_add(k).checked_shl(shift).unwrap_or(u32::MAX) as usize;
                let length = u8::try_from(l).unwrap_or(u8::MAX);
                for e in self.table.iter_mut().skip(start).take(1 << shift) {
                    *e = (symbol, length);
                }
            }
        }
        Ok(())
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16> {
        if self.sorted.is_empty() {
            return Err(invalid(
                CODEC,
                bits.at(),
                format!("a symbol of the empty {} tree", self.name),
            ));
        }
        let prefix = bits.peek(self.table_bits) as usize;
        if let Some(&(symbol, length)) = self.table.get(prefix)
            && length != 0
        {
            bits.consume(u32::from(length))?;
            return Ok(symbol);
        }
        let all = bits.peek(MAX_CODE);
        for l in self.table_bits.saturating_add(1)..=MAX_CODE {
            let li = l as usize;
            let code = all.checked_shr(MAX_CODE.saturating_sub(l)).unwrap_or(0);
            let (first, n) = (at_length(&self.first, li), at_length(&self.count, li));
            if let Some(k) = code.checked_sub(first)
                && k < n
            {
                bits.consume(l)?;
                return self
                    .sorted
                    .get(at_length(&self.index, li).saturating_add(k) as usize)
                    .copied()
                    .ok_or_else(|| {
                        invalid(
                            CODEC,
                            bits.at(),
                            format!("{} tree: no symbol for a code", self.name),
                        )
                    });
            }
        }
        Err(invalid(
            CODEC,
            bits.at(),
            format!("{} tree: no code matches", self.name),
        ))
    }
}

/// A per-length table's entry for code length `l` (0 past the longest).
fn at_length(t: &[u32; MAX_CODE as usize + 1], l: usize) -> u32 {
    t.get(l).copied().unwrap_or(0)
}

/// An LZX stream's decoder, fed one frame at a time (the module documentation).
#[derive(Clone)]
pub struct LzxDecoder {
    window: Vec<u8>,
    /// `window.len() - 1` (a power of two less one).
    mask: usize,
    /// Where the next byte goes in the window.
    position: usize,
    /// Where the current frame starts in the window.
    frame_start: usize,
    /// Bytes of output before the current frame.
    output: u64,
    frames: u64,
    main_symbols: usize,
    repeated: [u32; 3],
    header_read: bool,
    e8_size: i32,
    e8_started: bool,
    block_type: u32,
    block_length: u32,
    block_left: u32,
    pretree: Tree,
    main: Tree,
    length: Tree,
    aligned: Tree,
    frame: Vec<u8>,
}

impl std::fmt::Debug for LzxDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LzxDecoder")
            .field("window", &self.window.len())
            .field("output", &self.output)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl LzxDecoder {
    /// A decoder for a stream whose window is `2^window_bits` bytes (15 to 21; XNA's content uses 16).
    pub fn new(window_bits: u32) -> Result<Self> {
        let slots = match window_bits {
            15..=19 => window_bits.saturating_mul(2),
            20 => 42,
            21 => 50,
            b => {
                return Err(invalid(
                    CODEC,
                    0,
                    format!("a window of 2^{b} bytes (LZX's are 2^15 to 2^21)"),
                ));
            }
        };
        let size = 1usize << window_bits;
        let mut window = Vec::new();
        window.try_reserve_exact(size).map_err(|_| Error::Limit)?;
        window.resize(size, 0);
        let mut main = Tree::new("main", MAIN_MAX, 12);
        main.symbols = LITERALS.saturating_add((slots as usize).saturating_mul(8));
        Ok(LzxDecoder {
            window,
            mask: size.saturating_sub(1),
            position: 0,
            frame_start: 0,
            output: 0,
            frames: 0,
            main_symbols: main.symbols,
            repeated: [1, 1, 1],
            header_read: false,
            e8_size: 0,
            e8_started: false,
            block_type: 0,
            block_length: 0,
            block_left: 0,
            pretree: Tree::new("pre", PRETREE_SYMBOLS, 6),
            main,
            length: Tree::new("length", SECONDARY_LENGTHS, 12),
            aligned: Tree::new("aligned offset", ALIGNED_SYMBOLS, 7),
            frame: Vec::with_capacity(LZX_FRAME),
        })
    }

    /// Read code lengths `first..last` of `tree` as deltas against its previous ones, through the pretree.
    fn lengths(
        pretree: &mut Tree,
        tree: &mut Tree,
        bits: &mut Bits,
        first: usize,
        last: usize,
    ) -> Result<()> {
        let at = bits.at();
        for x in 0..PRETREE_SYMBOLS {
            let l = bits.read(4)?;
            pretree.set_length(x, l, at)?;
        }
        pretree.build(at)?;
        let mut x = first;
        while x < last {
            let at = bits.at();
            let z = u32::from(pretree.decode(bits)?);
            let (run, delta) = match z {
                17 => (bits.read(4)?.saturating_add(4), None),
                18 => (bits.read(5)?.saturating_add(20), None),
                19 => {
                    let run = bits.read(1)?.saturating_add(4);
                    let d = u32::from(pretree.decode(bits)?);
                    if d > 16 {
                        return Err(invalid(
                            CODEC,
                            at,
                            format!("{} tree: a run of the pretree's symbol {d}", tree.name),
                        ));
                    }
                    (run, Some(d))
                }
                d => (1, Some(d)),
            };
            // a delta is taken mod 17 against the length at the run's start, and the whole run gets that length
            let old = u32::from(tree.lengths.get(x).copied().unwrap_or(0));
            let new = delta.map_or(0, |d| old.saturating_add(17).saturating_sub(d) % 17);
            for _ in 0..run {
                tree.set_length(x, new, at)?;
                x = x.saturating_add(1);
            }
        }
        Ok(())
    }

    fn block_header(&mut self, bits: &mut Bits) -> Result<()> {
        if self.block_type == UNCOMPRESSED {
            // the bit stream starts afresh after stored bytes, past the byte that pads an odd block
            let at = bits.at();
            if self.block_length % 2 == 1 {
                bits.bytes(1).map_err(|_| {
                    invalid(
                        CODEC,
                        at,
                        "the byte that pads an odd uncompressed block is missing",
                    )
                })?;
            }
        }
        let at = bits.at();
        self.block_type = bits.read(3)?;
        let hi = bits.read(16)?;
        let lo = bits.read(8)?;
        self.block_length = hi.checked_shl(8).unwrap_or(0) | lo;
        self.block_left = self.block_length;
        match self.block_type {
            VERBATIM | ALIGNED => {
                if self.block_type == ALIGNED {
                    for i in 0..ALIGNED_SYMBOLS {
                        let l = bits.read(3)?;
                        self.aligned.set_length(i, l, at)?;
                    }
                    self.aligned.build(at)?;
                }
                let main_symbols = self.main_symbols;
                Self::lengths(&mut self.pretree, &mut self.main, bits, 0, LITERALS)?;
                Self::lengths(&mut self.pretree, &mut self.main, bits, LITERALS, main_symbols)?;
                self.main.build(at)?;
                if self.main.lengths.get(E8).is_some_and(|&l| l != 0) {
                    self.e8_started = true;
                }
                Self::lengths(&mut self.pretree, &mut self.length, bits, 0, SECONDARY_LENGTHS)?;
                self.length.build(at)?;
            }
            UNCOMPRESSED => {
                self.e8_started = true;
                bits.align()?;
                let stored = bits.bytes(12)?;
                for (slot, &v) in self.repeated.iter_mut().zip(stored.as_chunks::<4>().0) {
                    let v = u32::from_le_bytes(v);
                    if v == 0 || usize::try_from(v).map_or(true, |v| v > self.window.len()) {
                        return Err(invalid(
                            CODEC,
                            at,
                            format!("a repeated offset of {v} in an uncompressed block"),
                        ));
                    }
                    *slot = v;
                }
            }
            t => return Err(invalid(CODEC, at, format!("block type {t}"))),
        }
        Ok(())
    }

    /// Bytes made since the stream's start.
    fn made(&self) -> u64 {
        let in_frame = self.position.wrapping_sub(self.frame_start) & self.mask;
        self.output
            .saturating_add(u64::try_from(in_frame).unwrap_or(u64::MAX))
    }

    fn push(&mut self, byte: u8) -> Result<()> {
        let slot = self.window.get_mut(self.position).ok_or(Error::Limit)?;
        *slot = byte;
        self.position = self.position.saturating_add(1);
        Ok(())
    }

    /// Copy a match of `length` bytes from `distance` back to the window's position.
    fn copy(&mut self, distance: u32, length: usize, at: usize) -> Result<()> {
        let back = usize::try_from(distance).map_err(|_| Error::Limit)?;
        let made = self.made();
        if back == 0 || back > self.window.len() || u64::from(distance) > made {
            return Err(invalid(
                CODEC,
                at,
                format!("a match {back} bytes back, after {made} bytes"),
            ));
        }
        if self
            .position
            .checked_add(length)
            .is_none_or(|end| end > self.window.len())
        {
            return Err(invalid(CODEC, at, "a match runs over the window's end"));
        }
        let mut from = self.position.wrapping_sub(back) & self.mask;
        for _ in 0..length {
            let b = self.window.get(from).copied().ok_or(Error::Limit)?;
            self.push(b)?;
            from = from.wrapping_add(1) & self.mask;
        }
        Ok(())
    }

    /// Decode literals and matches of the current (verbatim or aligned) block until at least `run` bytes are made;
    /// the bytes made past `run` (a match that runs on).
    fn decode_run(&mut self, bits: &mut Bits, run: usize) -> Result<usize> {
        let mut made = 0usize;
        while made < run {
            let at = bits.at();
            let element = usize::from(self.main.decode(bits)?);
            let Some(element) = element.checked_sub(LITERALS) else {
                self.push(u8::try_from(element).map_err(|_| Error::Limit)?)?;
                made = made.saturating_add(1);
                continue;
            };
            let mut length = element & 7;
            if length == PRIMARY_LENGTHS {
                length = length.saturating_add(usize::from(self.length.decode(bits)?));
            }
            length = length.saturating_add(MIN_MATCH);
            let slot = element >> 3;
            let distance = match slot {
                0 => self.repeated[0],
                1 => {
                    self.repeated.swap(0, 1);
                    self.repeated[0]
                }
                2 => {
                    self.repeated.swap(0, 2);
                    self.repeated[0]
                }
                _ => {
                    let (&extra, &base) = EXTRA_BITS
                        .get(slot)
                        .zip(POSITION_BASE.get(slot))
                        .ok_or_else(|| invalid(CODEC, at, format!("position slot {slot}")))?;
                    let base = base.saturating_sub(2);
                    let offset = if self.block_type == ALIGNED && extra >= 3 {
                        let verbatim = bits.read(extra.saturating_sub(3))?;
                        let aligned = u32::from(self.aligned.decode(bits)?);
                        base.saturating_add(verbatim.checked_shl(3).unwrap_or(0))
                            .saturating_add(aligned)
                    } else {
                        base.saturating_add(bits.read(extra)?)
                    };
                    self.repeated = [offset, self.repeated[0], self.repeated[1]];
                    offset
                }
            };
            self.copy(distance, length, at)?;
            made = made.saturating_add(length);
        }
        Ok(made.saturating_sub(run))
    }

    /// Decode the next frame from `input`, its compressed bytes (on a bit stream of its own, as each of XNB's blocks
    /// is), to exactly `size` bytes (1 to [`LZX_FRAME`]).
    pub fn frame(&mut self, input: &[u8], size: usize) -> Result<&[u8]> {
        if size == 0 || size > LZX_FRAME {
            return Err(invalid(
                CODEC,
                0,
                format!("a frame of {size} bytes (LZX's are 1 to {LZX_FRAME})"),
            ));
        }
        let frame_end = self
            .frame_start
            .checked_add(size)
            .filter(|&e| e <= self.window.len())
            .ok_or_else(|| invalid(CODEC, 0, "a frame runs over the window's end"))?;
        let mut bits = Bits::new(input);
        if !self.header_read {
            if bits.read(1)? == 1 {
                let hi = bits.read(16)?;
                let lo = bits.read(16)?;
                self.e8_size = i32::from_be_bytes((hi.checked_shl(16).unwrap_or(0) | lo).to_be_bytes());
            }
            self.header_read = true;
        }
        // a match of the last frame may have made the first bytes of this one
        let mut todo = frame_end
            .checked_sub(self.position)
            .ok_or_else(|| invalid(CODEC, 0, "the last frame's match ran past this whole frame"))?;
        while todo > 0 {
            if self.block_left == 0 {
                self.block_header(&mut bits)?;
                continue;
            }
            let at = bits.at();
            let run = usize::try_from(self.block_left).unwrap_or(usize::MAX).min(todo);
            let over = if self.block_type == UNCOMPRESSED {
                let stored = bits.bytes(run)?;
                let end = self.position.saturating_add(run);
                self.window
                    .get_mut(self.position..end)
                    .ok_or_else(|| invalid(CODEC, at, "stored bytes run over the window's end"))?
                    .copy_from_slice(stored);
                self.position = end;
                0
            } else {
                self.decode_run(&mut bits, run)?
            };
            let used = u32::try_from(run.saturating_add(over)).map_err(|_| Error::Limit)?;
            self.block_left = self
                .block_left
                .checked_sub(used)
                .ok_or_else(|| invalid(CODEC, at, "a match runs past the end of its block"))?;
            todo = todo.saturating_sub(run);
        }
        if bits.left() > 0 {
            return Err(invalid(
                CODEC,
                bits.at(),
                format!("{} bytes after the frame", bits.left()),
            ));
        }
        self.frame.clear();
        self.frame
            .extend_from_slice(self.window.get(self.frame_start..frame_end).ok_or(Error::Limit)?);
        if self.e8_started && self.e8_size != 0 && self.frames < E8_FRAMES && size > 10 {
            translate_e8(&mut self.frame, self.output, self.e8_size);
        }
        self.frames = self.frames.saturating_add(1);
        self.output = self
            .output
            .saturating_add(u64::try_from(size).unwrap_or(u64::MAX));
        self.frame_start = frame_end & self.mask;
        if self.position == self.window.len() {
            self.position = 0;
        }
        Ok(&self.frame)
    }
}

/// Undo x86 call translation over one frame that starts `offset` bytes into the output.
fn translate_e8(frame: &mut [u8], offset: u64, file_size: i32) {
    let end = frame.len().saturating_sub(10);
    let mut current = i64::try_from(offset).unwrap_or(i64::MAX);
    let mut i = 0usize;
    while i < end {
        if frame.get(i) != Some(&0xE8) {
            i = i.saturating_add(1);
            current = current.saturating_add(1);
            continue;
        }
        let Some(b) = i
            .checked_add(1)
            .and_then(|s| frame.get_mut(s..s.saturating_add(4)))
        else {
            break;
        };
        let Ok(word) = <[u8; 4]>::try_from(&*b) else {
            break;
        };
        let absolute = i64::from(i32::from_le_bytes(word));
        if absolute >= current.saturating_neg() && absolute < i64::from(file_size) {
            let relative = if absolute >= 0 {
                absolute.saturating_sub(current)
            } else {
                absolute.saturating_add(i64::from(file_size))
            };
            b.copy_from_slice(&i32::try_from(relative).unwrap_or(0).to_le_bytes());
        }
        i = i.saturating_add(5);
        current = current.saturating_add(5);
    }
}
