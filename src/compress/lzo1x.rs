//! LZO1X (Markus Oberhumer's LZO, `lzo1x_decompress_safe` of LZO 2.10, the format miniLZO also writes): a byte-oriented
//! LZ77 whose instructions are whole bytes.
//!
//! An instruction byte `t` means, by its range and by what came before it:
//! - `t < 16` after a match's trailing literals ("M1"): a 2-byte match, distance `1 + (t >> 2) + (b << 2)` with `b` the
//!   next byte;
//! - `t < 16` right after a literal run: a 3-byte match, distance `1 + 0x800 + (t >> 2) + (b << 2)`;
//! - `t < 16` anywhere else (the stream's start, or after a match with no trailing literals): a literal run of
//!   `t + 3` bytes, or for `t = 0` of `18 + 255 z + n` bytes, after `z` zero bytes and the byte `n` (non-zero);
//! - `64 <= t` ("M2"): a match of `(t >> 5) + 1` bytes (3 to 8), distance `1 + ((t >> 2) & 7) + (b << 3)`;
//! - `32 <= t < 64` ("M3"): a match of `(t & 31) + 2` bytes (for `t & 31 = 0`, `33 + 255 z + n`, counted as for a
//!   literal run), then two bytes `b0 b1`: distance `1 + (b0 >> 2) + (b1 << 6)`;
//! - `16 <= t < 32` ("M4"): a match of `(t & 7) + 2` bytes (for `t & 7 = 0`, `9 + 255 z + n`), then `b0 b1`: distance
//!   `0x4000 + ((t & 8) << 11) + (b0 >> 2) + (b1 << 6)`; the distance part `((t & 8) << 11) + (b0 >> 2) + (b1 << 6)`
//!   being 0 ends the stream (the usual end code is `11 00 00`).
//!
//! After each match, the low two bits of the byte two before the next instruction (the M1, M2 or literal-run match's
//! instruction byte, the M3 or M4 match's `b0`) are 0 to 3 literal bytes that follow it. A first byte above 17 is a
//! literal run of `byte - 17` bytes; when that is under 4 the run counts as a match's trailing literals.
//!
//! Strict as the crate's other codecs: the end code is required, every input byte must be consumed by then, a match
//! may not reach before the output's start, and the output must be exactly the expected size.

use crate::compress::{Decompressor, Error, LZO1X_MAX_RATIO, Result, check_claim, invalid};

const CODEC: &str = "lzo1x";

/// LZO1X, one complete stream (the module documentation).
#[derive(Debug, Clone, Copy)]
pub struct Lzo1x;

/// The sum of an instruction's fields. Each is below 2^17 (a byte shifted by at most 11, or 0x4000), so the sum is
/// exact.
fn sum<const N: usize>(fields: [usize; N]) -> usize {
    fields.into_iter().fold(0, usize::saturating_add)
}

/// What an instruction byte below 16 means: it depends on what came before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    /// The stream's start, or a match with no trailing literals: a literal run.
    Nothing,
    /// A literal run: a 3-byte match beyond the M2 range.
    LiteralRun,
    /// A match's trailing literals: a 2-byte match (M1).
    TrailingLiterals,
}

struct Stream<'a> {
    input: &'a [u8],
    at: usize,
    out: Vec<u8>,
    expected: usize,
}

impl Stream<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.input.get(self.at).ok_or_else(|| {
            invalid(
                CODEC,
                self.at,
                "stream ends before its end code: expected another byte",
            )
        })?;
        // `at` indexed a byte of the input, so one more cannot wrap
        self.at = self.at.saturating_add(1);
        Ok(b)
    }

    /// A length extension: each zero byte adds 255, the first non-zero byte `n` adds `n` and ends it.
    fn extended(&mut self, base: usize) -> Result<usize> {
        let mut length = base;
        loop {
            match self.byte()? {
                0 => length = length.checked_add(255).ok_or(Error::Limit)?,
                n => return length.checked_add(usize::from(n)).ok_or(Error::Limit),
            }
        }
    }

    fn grow_to(&self, length: usize) -> Result<usize> {
        let end = self.out.len().checked_add(length).ok_or(Error::Limit)?;
        if end > self.expected {
            return Err(Error::Size {
                expected: self.expected,
                actual: end,
            });
        }
        Ok(end)
    }

    fn literals(&mut self, length: usize) -> Result<()> {
        self.grow_to(length)?;
        let from = self.at;
        let bytes = self
            .at
            .checked_add(length)
            .and_then(|end| self.input.get(from..end))
            .ok_or_else(|| {
                invalid(
                    CODEC,
                    from,
                    format!(
                        "literal run of {length} bytes: only {} left",
                        self.input.len().saturating_sub(from)
                    ),
                )
            })?;
        self.out.extend_from_slice(bytes);
        self.at = self.at.saturating_add(length);
        Ok(())
    }

    /// Copies `length` bytes from `distance` back; `operands` is where the match's instruction began, for errors.
    fn copy(&mut self, length: usize, distance: usize, operands: usize) -> Result<()> {
        let from = self
            .out
            .len()
            .checked_sub(distance)
            .filter(|_| distance > 0)
            .ok_or_else(|| {
                invalid(
                    CODEC,
                    operands,
                    format!(
                        "match {distance} bytes back at output position {}: expected at most {}",
                        self.out.len(),
                        self.out.len()
                    ),
                )
            })?;
        self.grow_to(length)?;
        // the match may overlap what it writes (distance < length repeats a run): copy byte by byte
        for k in from..from.saturating_add(length) {
            let b = *self
                .out
                .get(k)
                .ok_or_else(|| invalid(CODEC, operands, "match reads past the output"))?;
            self.out.push(b);
        }
        Ok(())
    }
}

impl Decompressor for Lzo1x {
    fn decompress(&self, input: &[u8], expected: usize) -> Result<Vec<u8>> {
        check_claim(CODEC, input.len(), expected, LZO1X_MAX_RATIO)?;
        let mut out: Vec<u8> = Vec::new();
        out.try_reserve_exact(expected).map_err(|_| Error::Limit)?;
        let mut s = Stream {
            input,
            at: 0,
            out,
            expected,
        };
        let mut after = After::Nothing;
        if let Some(&first) = input.first()
            && first > 17
        {
            s.at = 1;
            let run = usize::from(first.saturating_sub(17));
            s.literals(run)?;
            after = if run < 4 {
                After::TrailingLiterals
            } else {
                After::LiteralRun
            };
        }
        loop {
            let operands = s.at;
            let t = s.byte()?;
            // (length, distance, the byte whose low two bits count the trailing literals)
            let (length, distance, trailing) = if t < 16 {
                match after {
                    After::Nothing => {
                        let run = if t == 0 {
                            s.extended(18)?
                        } else {
                            sum([usize::from(t), 3])
                        };
                        s.literals(run)?;
                        after = After::LiteralRun;
                        continue;
                    }
                    After::LiteralRun => {
                        let b = s.byte()?;
                        (3, sum([1, 0x800, usize::from(t >> 2), usize::from(b) << 2]), t)
                    }
                    After::TrailingLiterals => {
                        let b = s.byte()?;
                        (2, sum([1, usize::from(t >> 2), usize::from(b) << 2]), t)
                    }
                }
            } else if t >= 64 {
                let b = s.byte()?;
                (
                    sum([usize::from(t >> 5), 1]),
                    sum([1, usize::from((t >> 2) & 7), usize::from(b) << 3]),
                    t,
                )
            } else if t >= 32 {
                let length = match t & 31 {
                    0 => s.extended(33)?,
                    n => sum([usize::from(n), 2]),
                };
                let b0 = s.byte()?;
                let b1 = s.byte()?;
                (length, sum([1, usize::from(b0 >> 2), usize::from(b1) << 6]), b0)
            } else {
                let length = match t & 7 {
                    0 => s.extended(9)?,
                    n => sum([usize::from(n), 2]),
                };
                let b0 = s.byte()?;
                let b1 = s.byte()?;
                let part = sum([
                    usize::from(t & 8) << 11,
                    usize::from(b0 >> 2),
                    usize::from(b1) << 6,
                ]);
                if part == 0 {
                    break;
                }
                (length, sum([part, 0x4000]), b0)
            };
            s.copy(length, distance, operands)?;
            match usize::from(trailing & 3) {
                0 => after = After::Nothing,
                n => {
                    s.literals(n)?;
                    after = After::TrailingLiterals;
                }
            }
        }
        if s.at != input.len() {
            return Err(invalid(
                CODEC,
                s.at,
                "input after the end code: expected the stream to end here",
            ));
        }
        if s.out.len() != expected {
            return Err(Error::Size {
                expected,
                actual: s.out.len(),
            });
        }
        Ok(s.out)
    }
}
