//! Bounds-checked reading of fixed-size numbers and byte spans from a borrowed slice. Every read either returns the
//! value or a [`BoundsError`] (or, once a name is attached with [`Field::field`], a [`FieldError`]) that says where the
//! read was and how far it ran past the data; nothing here panics on short input.
//!
//! [`View`] reads at explicit offsets; [`Cursor`] reads in sequence and moves only after a successful read. Both can
//! be given an `origin`, the offset of their first byte in the file they were cut from, so errors from a nested
//! structure still report the offset in the file. Format, allocation and count policies stay with the caller, and so
//! does the byte order: every multi-byte read states its [`Endian`].
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

use std::{fmt, ops::Range};

/// Why a read was refused. The operands are kept as given, so the offset in a message is exact even where adding the
/// origin would overflow `usize` (see [`BoundsError::absolute`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundsError {
    /// `count` entries of `stride` bytes, or `offset` plus their length, do not fit in a `usize`.
    LengthOverflow {
        /// Where the span starts, within the view.
        offset: usize,
        /// The file offset of the view's first byte.
        origin: usize,
        /// The number of entries asked for.
        count: usize,
        /// The size of each entry in bytes.
        stride: usize,
    },
    /// The span of `length` bytes at `offset` ends beyond the `total` bytes of the view.
    OutOfBounds {
        /// Where the span starts, within the view.
        offset: usize,
        /// The file offset of the view's first byte.
        origin: usize,
        /// The span's length in bytes.
        length: usize,
        /// The size of the view in bytes.
        total: usize,
    },
}
impl BoundsError {
    fn with_origin(self, origin: usize) -> Self {
        match self {
            Self::LengthOverflow {
                offset,
                count,
                stride,
                ..
            } => Self::LengthOverflow {
                offset,
                origin,
                count,
                stride,
            },
            Self::OutOfBounds {
                offset,
                length,
                total,
                ..
            } => Self::OutOfBounds {
                offset,
                origin,
                length,
                total,
            },
        }
    }
    /// Exact absolute byte offset, including an origin beyond the local view.
    pub fn absolute(&self) -> u128 {
        let (offset, origin) = match *self {
            Self::LengthOverflow { offset, origin, .. } | Self::OutOfBounds { offset, origin, .. } => {
                (offset, origin)
            }
        };
        // two `usize`s always sum within a `u128`; saturating only states that to the lint
        (offset as u128).saturating_add(origin as u128)
    }
}
impl fmt::Display for BoundsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let offset = self.absolute();
        match self {
            Self::LengthOverflow { count, stride, .. } => write!(
                f,
                "byte span at {offset:#x}: {count} entries of {stride} bytes overflow"
            ),
            Self::OutOfBounds { length, total, .. } => write!(
                f,
                "byte span at {offset:#x}: {length} bytes outside a {total}-byte view"
            ),
        }
    }
}
impl std::error::Error for BoundsError {}

/// A read of a named field that failed: the field, and where its bytes would be ([`Field::field`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldError {
    /// The name given to [`Field::field`].
    pub what: &'static str,
    /// The refused read.
    pub bounds: BoundsError,
}
impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, at) = (self.what, self.bounds.absolute());
        match self.bounds {
            BoundsError::LengthOverflow { count, stride, .. } => {
                write!(f, "{what} at {at:#x}: {count} entries of {stride} bytes overflow")
            }
            BoundsError::OutOfBounds { length, total, .. } => {
                write!(f, "{what} at {at:#x}: {length} bytes run past the end of {total}")
            }
        }
    }
}
impl std::error::Error for FieldError {}

/// Names the field a failed read was of.
pub trait Field<T> {
    /// Turns a refused read into a [`FieldError`] naming `what`.
    fn field(self, what: &'static str) -> Result<T, FieldError>;
}
impl<T> Field<T> for Result<T, BoundsError> {
    fn field(self, what: &'static str) -> Result<T, FieldError> {
        self.map_err(|bounds| FieldError { what, bounds })
    }
}

/// The byte order of a multi-byte number: the format's, stated at each read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// The fixed-size numbers [`View`] and [`Cursor`] read, in either byte order.
macro_rules! numbers {
    ($($name:ident: $t:ty),* $(,)?) => {
        impl View<'_> {
            $(
                #[doc = concat!("The `", stringify!($t), "` at `offset`, in byte order `e`.")]
                pub fn $name(&self, offset: usize, e: Endian) -> Result<$t, BoundsError> {
                    let b = self.array(offset)?;
                    Ok(match e {
                        Endian::Little => <$t>::from_le_bytes(b),
                        Endian::Big => <$t>::from_be_bytes(b),
                    })
                }
            )*
        }
        impl Cursor<'_> {
            $(
                #[doc = concat!("The next `", stringify!($t), "`, in byte order `e`.")]
                pub fn $name(&mut self, e: Endian) -> Result<$t, BoundsError> {
                    let b = self.array()?;
                    Ok(match e {
                        Endian::Little => <$t>::from_le_bytes(b),
                        Endian::Big => <$t>::from_be_bytes(b),
                    })
                }
            )*
        }
    };
}
numbers!(u16: u16, i16: i16, u32: u32, i32: i32, u64: u64, i64: i64, f32: f32, f64: f64);

/// The byte range of `count` entries of `stride` bytes starting at `offset` in a view of `total` bytes: the
/// multiplication, the addition and the bound are each checked, in that order. A zero stride makes every count fit,
/// so it does not by itself make allocating or processing `count` records safe; that is the caller's policy.
pub fn checked_span(
    offset: usize,
    count: usize,
    stride: usize,
    total: usize,
) -> Result<Range<usize>, BoundsError> {
    let overflow = || BoundsError::LengthOverflow {
        offset,
        origin: 0,
        count,
        stride,
    };
    let length = count.checked_mul(stride).ok_or_else(overflow)?;
    let end = offset.checked_add(length).ok_or_else(overflow)?;
    if end > total {
        return Err(BoundsError::OutOfBounds {
            offset,
            origin: 0,
            length,
            total,
        });
    }
    Ok(offset..end)
}

/// A borrowed byte slice read at explicit offsets, with the file offset of its first byte for error reporting.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    data: &'a [u8],
    origin: usize,
}
impl<'a> View<'a> {
    /// A view of `data`, whose first byte is offset 0 of the file.
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_origin(data, 0)
    }
    /// `origin` is the source byte offset of `data[0]`; slicing offsets remain local.
    pub fn with_origin(data: &'a [u8], origin: usize) -> Self {
        Self { data, origin }
    }
    /// The range of `length` bytes at `offset`, if the view holds it.
    fn locate(&self, offset: usize, length: usize) -> Result<Range<usize>, BoundsError> {
        checked_span(offset, length, 1, self.data.len()).map_err(|e| e.with_origin(self.origin))
    }
    /// The bytes of a range [`View::locate`] returned.
    fn bytes_in(&self, span: Range<usize>) -> Result<&'a [u8], BoundsError> {
        let (offset, length) = (span.start, span.len());
        self.data.get(span).ok_or(BoundsError::OutOfBounds {
            offset,
            origin: self.origin,
            length,
            total: self.data.len(),
        })
    }
    /// The `length` bytes at `offset`.
    pub fn slice(&self, offset: usize, length: usize) -> Result<&'a [u8], BoundsError> {
        self.bytes_in(self.locate(offset, length)?)
    }
    /// The `N` bytes at `offset`.
    pub fn array<const N: usize>(&self, offset: usize) -> Result<[u8; N], BoundsError> {
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.slice(offset, N)?);
        Ok(bytes)
    }
    /// The byte at `offset`.
    pub fn u8(&self, offset: usize) -> Result<u8, BoundsError> {
        self.array::<1>(offset).map(|[b]| b)
    }
    /// The byte at `offset`, signed.
    pub fn i8(&self, offset: usize) -> Result<i8, BoundsError> {
        self.array::<1>(offset).map(i8::from_le_bytes)
    }
}

/// A sequential reader over a borrowed slice. It advances only after a successful read or seek, so a refused read
/// leaves it where it was.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    view: View<'a>,
    position: usize,
}
impl<'a> Cursor<'a> {
    /// A cursor at the start of `data`, whose first byte is offset 0 of the file.
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_origin(data, 0)
    }
    /// A cursor at the start of `data`, whose first byte is offset `origin` of the file.
    pub fn with_origin(data: &'a [u8], origin: usize) -> Self {
        Self {
            view: View::with_origin(data, origin),
            position: 0,
        }
    }
    /// The offset of the next byte to read, within the cursor's data.
    pub fn position(&self) -> usize {
        self.position
    }
    /// All the bytes it reads, from its start (a parser that looks back or ahead by offset).
    pub fn bytes(&self) -> &'a [u8] {
        self.view.data
    }
    /// How many bytes are left after the position.
    pub fn remaining(&self) -> usize {
        // the position never passes the end, but a subtraction that cannot wrap is the cheaper proof
        self.view.data.len().saturating_sub(self.position)
    }
    /// The next `length` bytes.
    pub fn take(&mut self, length: usize) -> Result<&'a [u8], BoundsError> {
        let span = self.view.locate(self.position, length)?;
        let end = span.end;
        let bytes = self.view.bytes_in(span)?;
        self.position = end;
        Ok(bytes)
    }
    /// The bytes from the position up to `end` (an offset in the view), the cursor left at `end`: refused when `end`
    /// is before the position or past the view.
    pub fn take_to(&mut self, end: usize) -> Result<&'a [u8], BoundsError> {
        let within = View::with_origin(self.view.slice(0, end)?, self.view.origin);
        let bytes = within.slice(self.position, end.saturating_sub(self.position))?;
        self.position = end;
        Ok(bytes)
    }
    /// Moves to `offset` (the end of the data is allowed); an offset past it is refused.
    pub fn seek(&mut self, offset: usize) -> Result<(), BoundsError> {
        self.view.slice(offset, 0)?;
        self.position = offset;
        Ok(())
    }
    /// The next `N` bytes.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], BoundsError> {
        let span = self.view.locate(self.position, N)?;
        let end = span.end;
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.view.bytes_in(span)?);
        self.position = end;
        Ok(bytes)
    }
    /// The next byte.
    pub fn u8(&mut self) -> Result<u8, BoundsError> {
        self.array::<1>().map(|[b]| b)
    }
    /// The next byte, signed.
    pub fn i8(&mut self) -> Result<i8, BoundsError> {
        self.array::<1>().map(i8::from_le_bytes)
    }
    /// A `u32` count, in byte order `e`, of records of `each` bytes that the rest of the view holds; a count it
    /// can't hold is refused, its span's error naming where the records would start, and the cursor stays before the
    /// count. A zero `each` bounds nothing (`checked_span`).
    pub fn count(&mut self, e: Endian, each: usize) -> Result<usize, BoundsError> {
        let start = self.position;
        let n = self.u32(e)?;
        // past usize (a 16-bit target), the count is beyond any view: the largest is refused the same way
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        match checked_span(self.position, n, each, self.view.data.len()) {
            Ok(_) => Ok(n),
            Err(err) => {
                self.position = start;
                Err(err.with_origin(self.view.origin))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans_and_arithmetic_boundaries() {
        assert_eq!(checked_span(0, 0, 1, 0), Ok(0..0));
        assert_eq!(checked_span(4, 0, 7, 4), Ok(4..4));
        assert!(matches!(
            checked_span(5, 0, 1, 4),
            Err(BoundsError::OutOfBounds { .. })
        ));
        assert_eq!(checked_span(1, usize::MAX, 0, 1), Ok(1..1));
        assert!(matches!(
            checked_span(0, usize::MAX, 2, usize::MAX),
            Err(BoundsError::LengthOverflow { .. })
        ));
        assert!(matches!(
            checked_span(usize::MAX, 1, 1, usize::MAX),
            Err(BoundsError::LengthOverflow { .. })
        ));
        assert_eq!(View::new(&[1, 2, 3]).array::<2>(1), Ok([2, 3]));
        assert!(View::new(&[]).array::<1>(0).is_err());
    }
    #[test]
    fn origins_are_exact_even_past_address_space() {
        let view = View::with_origin(&[], usize::MAX);
        assert_eq!(view.slice(1, 0).unwrap_err().absolute(), usize::MAX as u128 + 1);
        assert_eq!(
            view.slice(1, 0),
            Err(BoundsError::OutOfBounds {
                offset: 1,
                origin: usize::MAX,
                length: 0,
                total: 0,
            })
        );
        assert_eq!(
            view.slice(usize::MAX, 1),
            Err(BoundsError::LengthOverflow {
                offset: usize::MAX,
                origin: usize::MAX,
                count: 1,
                stride: 1,
            })
        );
    }
    /// Numbers read in the stated byte order, positionally and in sequence; a failed read leaves the cursor where it
    /// was; a field's error names it and its exact offset.
    #[test]
    fn numbers_in_either_order_and_named_fields() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x80, 0x3f];
        let view = View::with_origin(&bytes, 0x100);
        assert_eq!(view.u16(0, Endian::Little), Ok(0x0201));
        assert_eq!(view.u16(0, Endian::Big), Ok(0x0102));
        assert_eq!(view.u32(0, Endian::Little), Ok(0x0403_0201));
        assert_eq!(view.i8(2), Ok(3));
        assert_eq!(view.f32(4, Endian::Little), Ok(1.0));
        assert_eq!(view.u64(0, Endian::Big), Ok(0x0102_0304_0000_803f));
        assert!(view.u32(6, Endian::Little).is_err());
        let mut c = Cursor::with_origin(&bytes, 0x100);
        assert_eq!(c.u8(), Ok(1));
        assert_eq!(c.u16(Endian::Big), Ok(0x0203));
        assert_eq!(c.position(), 3);
        let err = c.u64(Endian::Little).field("a header's size").unwrap_err();
        assert_eq!(c.position(), 3);
        assert_eq!(
            err.to_string(),
            "a header's size at 0x103: 8 bytes run past the end of 8"
        );
        assert_eq!(c.array::<5>(), Ok([0x04, 0x00, 0x00, 0x80, 0x3f]));
        // up to an end: not before the position, not past the view
        let mut c = Cursor::new(&bytes);
        c.seek(2).unwrap();
        assert_eq!(c.take_to(4), Ok(&[0x03, 0x04][..]));
        assert_eq!(c.position(), 4);
        assert!(c.take_to(3).is_err());
        assert!(c.take_to(9).is_err());
        assert_eq!(c.position(), 4);
    }

    /// A count the rest of the view holds is read; one it can't (or whose bytes overflow) is refused without
    /// advancing, naming where its records would start.
    #[test]
    fn counts_are_held_to_the_rest_of_the_view() {
        // a count of 2 records of 2 bytes, then 4 bytes
        let bytes = [2, 0, 0, 0, 9, 9, 9, 9];
        let mut c = Cursor::new(&bytes);
        assert_eq!(c.count(Endian::Little, 2), Ok(2));
        assert_eq!(c.position(), 4);
        let mut c = Cursor::with_origin(&bytes, 0x10);
        assert_eq!(
            c.count(Endian::Little, 3),
            Err(BoundsError::OutOfBounds {
                offset: 4,
                origin: 0x10,
                length: 6,
                total: 8,
            })
        );
        assert_eq!(c.position(), 0);
        let huge = [0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            Cursor::new(&huge).count(Endian::Little, usize::MAX),
            Err(BoundsError::LengthOverflow { .. })
        ));
        assert!(Cursor::new(&huge[..2]).count(Endian::Little, 1).is_err());
    }

    #[test]
    fn failed_cursor_operations_do_not_advance() {
        let mut cursor = Cursor::with_origin(&[1, 2, 3], 10);
        assert_eq!(cursor.take(2), Ok(&[1, 2][..]));
        assert!(cursor.take(2).is_err());
        assert!(cursor.take(usize::MAX).is_err());
        assert!(cursor.seek(4).is_err());
        assert_eq!(cursor.position(), 2);
        assert_eq!(cursor.remaining(), 1);
        assert_eq!(cursor.take(1), Ok(&[3][..]));
        assert_eq!(cursor.take(0), Ok(&[][..]));
        assert_eq!(cursor.seek(0), Ok(()));
        assert_eq!(cursor.remaining(), 3);
    }
}
