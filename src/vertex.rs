//! Decoding vertex data: DXGI element formats, layouts, vertex streams, quantized and packed attributes, and the
//! triangle-strip and index-buffer helpers a mesh converter needs.
//!
//! The arithmetic is done in `f32` in a fixed order of operations, without fused multiply-add and without any implicit
//! change of coordinate axes, so the same bytes give the same bits on every platform. Everything that can fail on
//! untrusted data returns an [`Error`]: an unsupported format, a non-finite decoded value (the `_raw` variants keep
//! those instead), an out-of-range index, or a stream too short for what was asked.
#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::BTreeMap;
/// Why vertex data was refused; the message says what was wrong.
#[derive(Debug, thiserror::Error)]
#[error("invalid vertex input: {0}")]
pub struct Error(pub String);
/// A result with this module's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
fn finite(v: &[f32]) -> Result<()> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(Error("non-finite value".into()))
    }
}
/// Supported DXGI element encodings. Unknown numeric IDs are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Format {
    /// `DXGI_FORMAT_R32G32B32A32_FLOAT`.
    Float4 = 2,
    /// `DXGI_FORMAT_R32G32B32_FLOAT`.
    Float3 = 6,
    /// `DXGI_FORMAT_R16G16B16A16_FLOAT`.
    Half4 = 10,
    /// `DXGI_FORMAT_R16G16B16A16_SNORM`.
    Snorm16x4 = 13,
    /// `DXGI_FORMAT_R32G32_FLOAT`.
    Float2 = 16,
    /// `DXGI_FORMAT_R10G10B10A2_UNORM`: three 10-bit channels and a 2-bit alpha, red in the low bits.
    Unorm10x3A2 = 24,
    /// `DXGI_FORMAT_R8G8B8A8_UNORM`.
    Unorm8x4 = 28,
    /// `DXGI_FORMAT_R16G16_FLOAT`.
    Half2 = 34,
    /// `DXGI_FORMAT_R16G16_UNORM`.
    Unorm16x2 = 35,
    /// `DXGI_FORMAT_R16G16_SNORM`.
    Snorm16x2 = 37,
}
impl Format {
    /// The format with `DXGI_FORMAT` number `id`; any number not listed in [`Format`] is refused.
    pub fn from_dxgi(id: u32) -> Result<Self> {
        Ok(match id {
            2 => Self::Float4,
            6 => Self::Float3,
            10 => Self::Half4,
            13 => Self::Snorm16x4,
            16 => Self::Float2,
            24 => Self::Unorm10x3A2,
            28 => Self::Unorm8x4,
            34 => Self::Half2,
            35 => Self::Unorm16x2,
            37 => Self::Snorm16x2,
            _ => return Err(Error(format!("unsupported DXGI format {id}"))),
        })
    }
    /// The element's size in bytes.
    pub fn size(self) -> usize {
        match self {
            Self::Float4 => 16,
            Self::Float3 => 12,
            Self::Half4 | Self::Snorm16x4 | Self::Float2 => 8,
            _ => 4,
        }
    }
    /// Decodes exactly one little-endian element (`data` must be [`Format::size`] bytes); components the format lacks
    /// are zero. NaN and infinity are refused.
    pub fn decode(self, data: &[u8]) -> Result<[f32; 4]> {
        let out = self.decode_raw(data)?;
        finite(&out)?;
        Ok(out)
    }
    /// [`Format::decode`] keeping NaN and infinity as stored, for a reader whose own rules say what a non-finite
    /// value means.
    pub fn decode_raw(self, data: &[u8]) -> Result<[f32; 4]> {
        if data.len() != self.size() {
            return Err(Error(format!(
                "expected {} element bytes, got {}",
                self.size(),
                data.len()
            )));
        }
        let mut out = [0.; 4];
        match self {
            Self::Float2 | Self::Float3 | Self::Float4 => {
                for (i, b) in data.as_chunks::<4>().0.iter().enumerate() {
                    out[i] = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
            }
            Self::Half2 | Self::Half4 => {
                for (i, b) in data.as_chunks::<2>().0.iter().enumerate() {
                    out[i] = half::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
                }
            }
            Self::Snorm16x2 | Self::Snorm16x4 => {
                for (i, b) in data.as_chunks::<2>().0.iter().enumerate() {
                    out[i] = snorm16(i16::from_le_bytes([b[0], b[1]]));
                }
            }
            Self::Unorm16x2 => {
                for (i, b) in data.as_chunks::<2>().0.iter().enumerate() {
                    out[i] = u16::from_le_bytes([b[0], b[1]]) as f32 / 65535.;
                }
            }
            Self::Unorm8x4 => {
                for i in 0..4 {
                    out[i] = data[i] as f32 / 255.;
                }
            }
            Self::Unorm10x3A2 => {
                let p = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                out = [
                    (p & 1023) as f32 / 1023.,
                    ((p >> 10) & 1023) as f32 / 1023.,
                    ((p >> 20) & 1023) as f32 / 1023.,
                    (p >> 30) as f32 / 3.,
                ];
            }
        }
        Ok(out)
    }
}
/// D3D signed normalization, including -32768 mapping to -1.
pub fn snorm16(v: i16) -> f32 {
    (v as f32 / 32767.).max(-1.)
}
/// One element of a vertex layout, as an input-layout description gives it.
///
/// Offsets are either explicit or appended. Each slot's append cursor sums the sizes of the slot's elements in
/// declaration order, independently of explicit offsets: an element with an explicit offset still advances the cursor
/// by its size. This differs from Direct3D's `D3D11_APPEND_ALIGNED_ELEMENT`, which continues from the end of the
/// previous element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// The semantic name, such as `POSITION`.
    pub semantic: String,
    /// The semantic index.
    pub index: u32,
    /// How the element is stored.
    pub format: Format,
    /// The vertex buffer slot the element is read from.
    pub slot: u32,
    /// The element's byte offset in its vertex, or `None` to append it to its slot.
    pub offset: Option<usize>,
}
/// An [`Element`] with its byte offset in the vertex worked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedElement {
    /// The element as declared.
    pub element: Element,
    /// The element's byte offset in its vertex.
    pub offset: usize,
}
/// Works out every element's offset (see [`Element`]); a layout whose offsets overflow `usize` is refused.
pub fn resolve_layout(elements: &[Element]) -> Result<Vec<ResolvedElement>> {
    let mut cursors = BTreeMap::<u32, usize>::new();
    let mut result = Vec::new();
    for e in elements {
        let cur = cursors.entry(e.slot).or_default();
        let offset = e.offset.unwrap_or(*cur);
        *cur = cur
            .checked_add(e.format.size())
            .ok_or_else(|| Error("layout offset overflow".into()))?;
        offset
            .checked_add(e.format.size())
            .ok_or_else(|| Error("element end overflow".into()))?;
        result.push(ResolvedElement {
            element: e.clone(),
            offset,
        });
    }
    Ok(result)
}
/// A vertex buffer bound to a slot.
#[derive(Clone, Copy, Debug)]
pub struct Stream<'a> {
    /// The buffer's bytes.
    pub bytes: &'a [u8],
    /// The size of one vertex in bytes; padding after the last element is allowed.
    pub stride: usize,
    /// The byte offset in `bytes` of the first vertex.
    pub offset: usize,
}
/// Decodes `count` consecutive vertices, from vertex `first`, of one resolved element in its stream. All arithmetic
/// and bounds are checked before anything is read: a zero stride, an element that does not fit the stride, a range
/// beyond the buffer, or a NaN or infinity in the data is refused.
pub fn decode_stream(
    element: &ResolvedElement,
    stream: Stream<'_>,
    first: usize,
    count: usize,
) -> Result<Vec<[f32; 4]>> {
    stream_with(element, stream, first, count, Format::decode)
}
/// [`decode_stream`] keeping NaN and infinity as stored ([`Format::decode_raw`]); bounds are checked the same.
pub fn decode_stream_raw(
    element: &ResolvedElement,
    stream: Stream<'_>,
    first: usize,
    count: usize,
) -> Result<Vec<[f32; 4]>> {
    stream_with(element, stream, first, count, Format::decode_raw)
}
fn stream_with(
    element: &ResolvedElement,
    stream: Stream<'_>,
    first: usize,
    count: usize,
    decode: fn(Format, &[u8]) -> Result<[f32; 4]>,
) -> Result<Vec<[f32; 4]>> {
    let size = element.element.format.size();
    let end = element
        .offset
        .checked_add(size)
        .ok_or_else(|| Error("offset overflow".into()))?;
    if stream.stride == 0 || end > stream.stride {
        return Err(Error("element extends beyond stride".into()));
    }
    let start = first
        .checked_mul(stream.stride)
        .and_then(|x| x.checked_add(stream.offset))
        .and_then(|x| x.checked_add(element.offset))
        .ok_or_else(|| Error("stream offset overflow".into()))?;
    let last = if count == 0 {
        start
    } else {
        (count - 1)
            .checked_mul(stream.stride)
            .and_then(|x| x.checked_add(start))
            .and_then(|x| x.checked_add(size))
            .ok_or_else(|| Error("stream end overflow".into()))?
    };
    if last > stream.bytes.len() {
        return Err(Error(format!(
            "stream needs {last} bytes, has {}",
            stream.bytes.len()
        )));
    }
    (0..count)
        .map(|i| {
            let off = start + i * stream.stride;
            decode(element.element.format, &stream.bytes[off..off + size])
        })
        .collect()
}
/// Per-axis `v * scale + translate`, rounded at each f32 operation (not fused). Non-finite inputs or results are
/// refused.
pub fn scale_translate(v: [f32; 3], scale: [f32; 3], translate: [f32; 3]) -> Result<[f32; 3]> {
    finite(&v)?;
    finite(&scale)?;
    finite(&translate)?;
    let r = std::array::from_fn(|i| v[i] * scale[i] + translate[i]);
    finite(&r)?;
    Ok(r)
}
/// Transforms a UV pair by `t = [u offset, u scale, v offset, v scale]` (`u * scale + offset`), then flips V (`1 - v`).
/// Non-finite inputs or results are refused.
pub fn transform_uv(uv: [f32; 2], t: [f32; 4]) -> Result<[f32; 2]> {
    finite(&uv)?;
    finite(&t)?;
    let r = [uv[0] * t[1] + t[0], 1. - (uv[1] * t[3] + t[2])];
    finite(&r)?;
    Ok(r)
}
/// Normalizes in f32; a zero vector stays zero. A non-finite input, or a length that overflows, is refused.
pub fn normalize(v: [f32; 3]) -> Result<[f32; 3]> {
    finite(&v)?;
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if !len.is_finite() {
        return Err(Error("normal length overflow".into()));
    }
    Ok(if len > 0. { v.map(|x| x / len) } else { v })
}
/// Decodes a packed R10G10B10A2 tangent: the 10-bit channels remapped to [-1, 1] and normalized, with the handedness
/// `w` +1 when bit 31 is set and -1 otherwise. Bit 30 is ignored.
pub fn tangent(p: u32) -> Result<[f32; 4]> {
    let n = normalize([
        (p & 1023) as f32 / 1023. * 2. - 1.,
        ((p >> 10) & 1023) as f32 / 1023. * 2. - 1.,
        ((p >> 20) & 1023) as f32 / 1023. * 2. - 1.,
    ])?;
    Ok([n[0], n[1], n[2], if p >> 31 != 0 { 1. } else { -1. }])
}
/// Rebuilds a normal stored as two SNORM16 components `yz`, whose x was stored in the position's w: `[position_w, y, z]`
/// normalized.
pub fn split_normal(position_w: f32, yz: [i16; 2]) -> Result<[f32; 3]> {
    normalize([position_w, snorm16(yz[0]), snorm16(yz[1])])
}
/// Unpacks a position stored as 11, 11 and 10 bits (x in the low bits) and applies `scale` and `offset` per axis
/// ([`scale_translate`]).
pub fn packed_position32(p: u32, scale: [f32; 3], offset: [f32; 3]) -> Result<[f32; 3]> {
    scale_translate(
        [(p & 0x7ff) as f32, ((p >> 11) & 0x7ff) as f32, (p >> 22) as f32],
        scale,
        offset,
    )
}
/// Unpacks a position stored as 21, 21 and 22 bits (x in the low bits) within the inclusive box `min..=max`. The scale
/// per axis is `(max - min)` divided by the field's maximum (2^21 - 1, 2^21 - 1, 2^22 - 1), all in f32.
pub fn packed_position64(p: u64, min: [f32; 3], max: [f32; 3]) -> Result<[f32; 3]> {
    finite(&min)?;
    finite(&max)?;
    let scale = [
        (max[0] - min[0]) / 2097151.,
        (max[1] - min[1]) / 2097151.,
        (max[2] - min[2]) / 4194303.,
    ];
    scale_translate(
        [
            (p & 0x1fffff) as f32,
            ((p >> 21) & 0x1fffff) as f32,
            (p >> 42) as f32,
        ],
        scale,
        min,
    )
}
/// Converts a triangle strip to a triangle list. Winding alternates per strip position; a triangle with a repeated index
/// (a degenerate joint) is dropped but still advances the alternation. With `reverse`, the first triangle is
/// `[v1, v0, v2]`. There is no primitive-restart sentinel.
pub fn strip_to_list(strip: &[u32], reverse: bool) -> Vec<u32> {
    let mut out = Vec::new();
    for (i, t) in strip.windows(3).enumerate() {
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        if (i % 2 == 1) ^ reverse {
            out.extend_from_slice(&[t[1], t[0], t[2]])
        } else {
            out.extend_from_slice(t)
        }
    }
    out
}
/// The integer width an index buffer is stored in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexWidth {
    /// 16-bit indices.
    U16,
    /// 32-bit indices.
    U32,
}
/// The narrowest width that holds every index: 16-bit up to and including index 65535.
pub fn index_width(indices: &[u32]) -> IndexWidth {
    if indices.iter().all(|&i| i <= u16::MAX as u32) {
        IndexWidth::U16
    } else {
        IndexWidth::U32
    }
}
/// One piece of a split mesh: the source vertices it uses and its triangles indexing into them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Chunk {
    /// The source vertex numbers the piece uses, in order of first use.
    pub vertices: Vec<u32>,
    /// The piece's triangle indices, as positions in `vertices`.
    pub indices: Vec<u32>,
}
/// Splits a triangle list into pieces of at most 65,535 vertices, for formats whose pieces use 16-bit indices. Before
/// each triangle the current piece is closed if its vertex count plus 3 would exceed the limit, even when that triangle
/// reuses vertices already in the piece. Triangles stay in order and unused source vertices are omitted. A triangle
/// list that is not whole triangles, or indexes a vertex at or past `vertex_count`, is refused.
pub fn split_triangles(indices: &[u32], vertex_count: usize) -> Result<Vec<Chunk>> {
    split_by_vertex_budget(indices, vertex_count, 65535)
}
/// [`split_triangles`] with a piece budget of `max_vertices`, which must be within 3..=65535.
pub fn split_by_vertex_budget(
    indices: &[u32],
    vertex_count: usize,
    max_vertices: usize,
) -> Result<Vec<Chunk>> {
    if !(3..=65535).contains(&max_vertices) {
        return Err(Error("vertex budget must be 3..=65535".into()));
    }
    if !indices.len().is_multiple_of(3) || indices.iter().any(|&i| i as usize >= vertex_count) {
        return Err(Error("invalid triangle indices".into()));
    }
    let mut chunks = Vec::new();
    let mut chunk = Chunk::default();
    let mut map = BTreeMap::new();
    for tri in indices.as_chunks::<3>().0 {
        if chunk.vertices.len() + 3 > max_vertices {
            chunks.push(std::mem::take(&mut chunk));
            map.clear();
        }
        for &v in tri {
            let next = chunk.vertices.len() as u32;
            let n = *map.entry(v).or_insert_with(|| {
                chunk.vertices.push(v);
                next
            });
            chunk.indices.push(n);
        }
    }
    if !chunk.indices.is_empty() {
        chunks.push(chunk)
    }
    Ok(chunks)
}

/// Keeps a stored normal whose length is within 1e-3 of one; otherwise divides it by its length, or gives +Z when the
/// length is 1e-12 or less. The length is the f32 sum of squares, its square root taken in f64 and rounded to f32. A
/// non-finite normal is refused.
pub fn renormalize_stored_normal(n: [f32; 3]) -> Result<[f32; 3]> {
    finite(&n)?;
    let len = ((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]) as f64).sqrt() as f32;
    finite(&[len])?;
    Ok(if (len - 1.).abs() > 1e-3 {
        if len > 1e-12 {
            n.map(|v| v / len)
        } else {
            [0., 0., 1.]
        }
    } else {
        n
    })
}
