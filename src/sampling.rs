//! Sampling a decoded texture: each policy the caller's, stated. Bilinear filtering between texel centres (a texel
//! `i` covering `[i, i + 1)`, `uv` scaled by the level's size, minus a half), each axis's address mode, the colour's
//! transfer before filtering, at a level the caller picks. Two precisions, each a fixed order of operations: f64
//! ([`Levels::bilinear`]) and f32 over texels decoded once ([`Decoded::bilinear`]). The texel a coordinate falls in
//! ([`Levels::point`], [`Decoded::point`]). A level of detail from a sample's footprint ([`footprint_lod`]), and the
//! two levels around it mixed ([`Levels::trilinear`]) or the nearest one taken ([`Levels::nearest_level`]).
//!
//! [`Decoded`] is one level of float texels, 2D or 3D: an RGBA8 level decoded once ([`Levels::decoded`]), or texels a
//! caller holds as floats ([`Decoded::from_texels`], [`Decoded::volume`]). A volume filters linearly between the eight
//! texels about a coordinate ([`Decoded::linear_3d`]) or takes the one it falls in ([`Decoded::point_3d`]).

use crate::color::srgb8_to_linear;

/// Why a sample or a texture's levels are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The texture or coordinate is invalid; the message says how.
    #[error("invalid image: {0}")]
    Invalid(String),
}

/// A result with this module's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Where a texel index outside the texture reads (the address modes of Direct3D's `D3D11_TEXTURE_ADDRESS_MODE`,
/// except border).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    /// Repeats the texture.
    Wrap,
    /// The edge texel.
    Clamp,
    /// Repeats the texture, every other copy mirrored.
    Mirror,
    /// Mirrored once about the origin, then the edge texel.
    MirrorOnce,
}

impl Address {
    fn index(self, i: i64, size: u32) -> usize {
        let n = i64::from(size.max(1));
        match self {
            Address::Wrap => i.rem_euclid(n) as usize,
            Address::Clamp => i.clamp(0, n - 1) as usize,
            // texel -1 is texel 0 mirrored, -2 is 1, ...: the pattern 0 1 .. n-1 n-1 .. 0 repeats every 2n texels
            Address::Mirror => {
                let m = i.rem_euclid(2 * n);
                (if m < n { m } else { 2 * n - 1 - m }) as usize
            }
            // mirrored once about the origin (-1 reads 0, -2 reads 1, ...), then the edge texel
            Address::MirrorOnce => (if i < 0 { -1 - i } else { i }).clamp(0, n - 1) as usize,
        }
    }
}

/// A texel channel's value before filtering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// `x / 255`.
    Stored,
    /// Red, green and blue decoded from sRGB to linear (as a GPU's sRGB view), alpha `x / 255`.
    SrgbColour,
}

impl Transfer {
    /// Channel `c` (0-3: red, green, blue, alpha) of a texel stored as `byte`, before filtering, in f64.
    pub fn channel(self, c: usize, byte: u8) -> f64 {
        match self {
            Transfer::SrgbColour if c < 3 => srgb8_to_linear(byte),
            _ => f64::from(byte) / 255.0,
        }
    }
}

/// An RGBA8 texture's mip levels, each stored row by row, four bytes a texel. Level `n` is the base size halved `n`
/// times (at least 1); [`Levels::size`] gives it.
#[derive(Clone, Debug)]
pub struct Levels {
    /// The base level's width in texels.
    pub width: u32,
    /// The base level's height in texels.
    pub height: u32,
    /// The RGBA8 bytes of each level, base level first. The fields are public, so the sampling methods check the
    /// level they read: one that is missing, or does not hold exactly its [`Levels::size`] in texels, is refused.
    pub levels: Vec<Vec<u8>>,
}

impl Levels {
    /// One level of RGBA8 texels.
    pub fn one(width: u32, height: u32, rgba: Vec<u8>) -> Result<Levels> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .filter(|&n| n > 0)
            .ok_or_else(|| Error::Invalid("sampled texture: invalid dimensions".into()))?;
        if count.checked_mul(4) != Some(rgba.len()) {
            return Err(Error::Invalid("sampled texture: RGBA length mismatch".into()));
        }
        Ok(Levels {
            width,
            height,
            levels: vec![rgba],
        })
    }

    /// Each level's rows in reverse order, for a texture stored bottom row first when row 0 is wanted at v = 0:
    /// addressing then applies about that texture's own origin. A level that isn't its size's whole rows is refused.
    pub fn rows_reversed(mut self) -> Result<Levels> {
        for i in 0..self.levels.len() {
            let row = self.level(i)?.row_bytes;
            let Some(level) = self.levels.get_mut(i) else {
                continue;
            };
            *level = level.chunks_exact(row).rev().flatten().copied().collect();
        }
        Ok(self)
    }

    /// Level `level`'s size: the base halved per level, at least 1.
    pub fn size(&self, level: usize) -> (u32, u32) {
        let s = u32::try_from(level).unwrap_or(31);
        (
            (self.width >> s.min(31)).max(1),
            (self.height >> s.min(31)).max(1),
        )
    }

    /// Level `level`'s bytes if it is stored and holds exactly its size's texels, else refused.
    fn level(&self, level: usize) -> Result<Level<'_>> {
        let (w, h) = self.size(level);
        let bytes = self.levels.get(level).ok_or_else(|| {
            Error::Invalid(format!(
                "sampled texture: level {level} requested, expected one of the {} stored levels",
                self.levels.len()
            ))
        })?;
        let row_bytes = (w as usize).checked_mul(4);
        let want = row_bytes.and_then(|r| r.checked_mul(h as usize));
        match (row_bytes, want) {
            (Some(row_bytes), Some(want)) if want == bytes.len() && want > 0 => Ok(Level {
                bytes,
                row_bytes,
                width: w,
                height: h,
            }),
            _ => Err(Error::Invalid(format!(
                "sampled texture: level {level} holds {} bytes, expected {h} rows of {w} RGBA8 texels",
                bytes.len()
            ))),
        }
    }

    /// The texel at (x, y) of level `level`, addressed by `address`. A level that is not stored, or does not hold
    /// its size's bytes, is refused.
    pub fn texel(&self, level: usize, x: i64, y: i64, address: [Address; 2]) -> Result<[u8; 4]> {
        self.level(level)?.texel(x, y, address)
    }

    /// Bilinear at `uv` of level `level`, in f64: weights `(1 - tx)(1 - ty)`, `tx (1 - ty)`, `(1 - tx) ty`, `tx ty`
    /// summed in that order. A coordinate that isn't finite or past an i32's range is refused.
    pub fn bilinear(
        &self,
        level: usize,
        uv: [f64; 2],
        address: [Address; 2],
        transfer: Transfer,
    ) -> Result<[f64; 4]> {
        let (w, h) = self.size(level);
        let level = self.level(level)?;
        let (x, y) = texel_coordinate([uv[0] * f64::from(w) - 0.5, uv[1] * f64::from(h) - 0.5])?;
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let (ix, iy) = (fx as i64, fy as i64);
        let mut out = [0.0; 4];
        for (dx, dy, wt) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            let t = level.texel(ix + dx, iy + dy, address)?;
            for (c, (o, byte)) in out.iter_mut().zip(t).enumerate() {
                *o += wt * transfer.channel(c, byte);
            }
        }
        Ok(out)
    }

    /// The texel of level `level` that `uv` falls in (texel `i` covering `[i, i + 1)` after scaling by the level's
    /// size), in f64: point filtering. A coordinate that isn't finite or past an i32's range is refused.
    pub fn point(
        &self,
        level: usize,
        uv: [f64; 2],
        address: [Address; 2],
        transfer: Transfer,
    ) -> Result<[f64; 4]> {
        let (w, h) = self.size(level);
        let level = self.level(level)?;
        let (x, y) = texel_coordinate([uv[0] * f64::from(w), uv[1] * f64::from(h)])?;
        let t = level.texel(x.floor() as i64, y.floor() as i64, address)?;
        Ok(std::array::from_fn(|c| {
            transfer.channel(c, t.get(c).copied().unwrap_or_default())
        }))
    }

    /// Level `level` decoded once to f32 by `transfer`, for [`Decoded::bilinear`]. A level that is not stored, or does
    /// not hold its size's bytes, is refused.
    pub fn decoded(&self, level: usize, transfer: Transfer) -> Result<Decoded> {
        let l = self.level(level)?;
        let texels = l
            .bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                std::array::from_fn(|c| match transfer {
                    Transfer::SrgbColour if c < 3 => srgb8_to_linear(p[c]) as f32,
                    _ => p[c] as f32 / 255.0,
                })
            })
            .collect();
        Ok(Decoded {
            width: l.width as usize,
            height: l.height as usize,
            depth: 1,
            texels,
        })
    }
}

/// One stored level that holds exactly its size's texels (checked by [`Levels::level`]).
struct Level<'a> {
    bytes: &'a [u8],
    row_bytes: usize,
    width: u32,
    height: u32,
}

impl Level<'_> {
    fn texel(&self, x: i64, y: i64, address: [Address; 2]) -> Result<[u8; 4]> {
        let (x, y) = (address[0].index(x, self.width), address[1].index(y, self.height));
        let i = y * self.row_bytes + x * 4;
        self.bytes
            .get(i..)
            .and_then(|rest| rest.first_chunk::<4>())
            .copied()
            .ok_or_else(|| {
                Error::Invalid(format!(
                    "sampled texture: texel ({x}, {y}) at byte {i}, expected within the level's {} bytes",
                    self.bytes.len()
                ))
            })
    }
}

/// A texel coordinate `[x, y]` that is finite and within an i32's range, else refused.
fn texel_coordinate([x, y]: [f64; 2]) -> Result<(f64, f64)> {
    if !x.is_finite() || !y.is_finite() || x.abs() >= 2147483520.0 || y.abs() >= 2147483520.0 {
        return Err(Error::Invalid(
            "sampled texture: nonfinite or excessive texel coordinate".into(),
        ));
    }
    Ok((x, y))
}

/// The level of detail of a footprint: the UV's screen derivatives `gradients` (d/dx, then d/dy) in texels of a
/// `width` x `height` base level, the longer one's base-2 logarithm (Direct3D's isotropic rule), plus `bias`. A
/// footprint of no extent gives negative infinity (the finest level, whatever the bias).
pub fn footprint_lod(width: u32, height: u32, gradients: [[f64; 2]; 2], bias: f64) -> f64 {
    let texels = |d: [f64; 2]| (d[0] * f64::from(width)).hypot(d[1] * f64::from(height));
    texels(gradients[0]).max(texels(gradients[1])).log2() + bias
}

impl Levels {
    /// Trilinear at `uv`: the level of detail `lod` clamped to the stored levels, the bilinear values
    /// ([`Levels::bilinear`]) of the level below it and the next mixed by its fraction (`a + (b - a) f`); a level
    /// of detail that isn't a number, or a coordinate [`Levels::bilinear`] refuses, is refused.
    pub fn trilinear(
        &self,
        uv: [f64; 2],
        lod: f64,
        address: [Address; 2],
        transfer: Transfer,
    ) -> Result<[f64; 4]> {
        if lod.is_nan() {
            return Err(Error::Invalid(
                "sampled texture: a level of detail that isn't a number".into(),
            ));
        }
        let last = self.levels.len().saturating_sub(1);
        let lod = lod.clamp(0.0, last as f64);
        let below = lod.floor();
        let (l0, f) = (below as usize, lod - below);
        let a = self.bilinear(l0, uv, address, transfer)?;
        if f == 0.0 {
            return Ok(a);
        }
        let b = self.bilinear((l0 + 1).min(last), uv, address, transfer)?;
        Ok(std::array::from_fn(|c| a[c] + (b[c] - a[c]) * f))
    }

    /// The stored level nearest the level of detail `lod`, as a point mip filter picks it: `floor(lod + 0.5)` (a half
    /// rounds up), clamped to the stored levels. A level of detail that isn't a number is refused.
    pub fn nearest_level(&self, lod: f64) -> Result<usize> {
        if lod.is_nan() {
            return Err(Error::Invalid(
                "sampled texture: a level of detail that isn't a number".into(),
            ));
        }
        let last = self.levels.len().saturating_sub(1);
        Ok((lod + 0.5).floor().clamp(0.0, last as f64) as usize)
    }
}

/// How finely a bilinear filter resolves where a sample falls between texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtexel {
    /// The position in f32, as computed.
    Exact,
    /// Direct3D 11's filtering: the texel coordinate (after scaling by the texture's size) truncated to 8 fractional
    /// bits, 256 subdivisions of a texel, before the weights are taken. The D3D11.3 functional specification requires
    /// at least 8 bits of sub-texel precision (7.18.16.1, Texture Addressing and LOD Precision;
    /// `D3D11_SUBTEXEL_FRACTIONAL_BIT_COUNT` = 8). Microsoft's WARP software rasterizer truncates at 8, as measured
    /// against it.
    D3d11,
}

impl Subtexel {
    /// `x` resolved to this precision: unchanged, or truncated to 1/256.
    fn resolve(self, x: f32) -> f32 {
        match self {
            Subtexel::Exact => x,
            Subtexel::D3d11 => (x * 256.).floor() / 256.,
        }
    }
}

/// One level's texels in f32, their transfer applied: a 2D level (`depth` 1) or a volume, row by row and slice by
/// slice.
#[derive(Clone, Debug)]
pub struct Decoded {
    width: usize,
    height: usize,
    depth: usize,
    texels: Vec<[f32; 4]>,
}

/// A texel coordinate in f32 that is finite and within an i32's range, else refused.
fn texel_coordinate_f32(x: f32) -> Result<f32> {
    if !x.is_finite() || x.abs() >= 2147483520.0 {
        return Err(Error::Invalid(
            "sampled texture: nonfinite or excessive texel coordinate".into(),
        ));
    }
    Ok(x)
}

impl Decoded {
    /// A 2D level of `width` x `height` float texels, row 0 first, as the caller holds them (no transfer: the values
    /// are filtered as they are). No texel, or a count other than the size's, is refused.
    pub fn from_texels(width: u32, height: u32, texels: Vec<[f32; 4]>) -> Result<Decoded> {
        Decoded::volume(width, height, 1, texels)
    }

    /// A volume of `width` x `height` x `depth` float texels, slice 0 first and each slice row 0 first. No texel, or a
    /// count other than the size's, is refused.
    pub fn volume(width: u32, height: u32, depth: u32, texels: Vec<[f32; 4]>) -> Result<Decoded> {
        let count = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(depth as usize))
            .filter(|&n| n > 0)
            .ok_or_else(|| Error::Invalid("sampled texture: invalid dimensions".into()))?;
        if texels.len() != count {
            return Err(Error::Invalid(format!(
                "sampled texture: {} texels for {width} x {height} x {depth}",
                texels.len()
            )));
        }
        Ok(Decoded {
            width: width as usize,
            height: height as usize,
            depth: depth as usize,
            texels,
        })
    }

    /// The level's `[width, height]` in texels.
    pub fn dimensions(&self) -> [usize; 2] {
        [self.width, self.height]
    }
    /// The slices of a volume: 1 for a 2D level.
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// The memory the decoded texels take, in bytes (16 a texel).
    pub fn decoded_bytes(&self) -> usize {
        self.texels.len() * 16
    }

    /// The texel at (x, y, z), if the level holds one there: no addressing, a load's view of the level.
    pub fn texel(&self, x: i64, y: i64, z: i64) -> Option<[f32; 4]> {
        let inside = |i: i64, n: usize| usize::try_from(i).ok().filter(|&i| i < n);
        let (x, y, z) = (
            inside(x, self.width)?,
            inside(y, self.height)?,
            inside(z, self.depth)?,
        );
        Some(self.texels[(z * self.height + y) * self.width + x])
    }

    fn at(&self, x: usize, y: usize, z: usize) -> [f32; 4] {
        self.texels[(z * self.height + y) * self.width + x]
    }

    /// A 2D sample of a volume is refused: which slice it reads is not the level's to say.
    fn plane(&self) -> Result<()> {
        if self.depth == 1 {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "sampled texture: a 2D sample of a volume of {} slices",
                self.depth
            )))
        }
    }

    /// Bilinear at `uv`, in f32 (the coordinate, the weights `(1 - t)` and `t` per axis, and the sum over the four
    /// texels row by row); a coordinate that isn't finite or past an i32's range is refused.
    pub fn bilinear(&self, uv: [f32; 2], address: [Address; 2]) -> Result<[f32; 4]> {
        self.bilinear_at(uv, address, Subtexel::Exact)
    }
    /// [`Decoded::bilinear`] with the texel coordinate resolved to `subtexel` precision before the weights are taken.
    pub fn bilinear_at(&self, uv: [f32; 2], address: [Address; 2], subtexel: Subtexel) -> Result<[f32; 4]> {
        self.plane()?;
        let x = texel_coordinate_f32(subtexel.resolve(uv[0] * self.width as f32 - 0.5))?;
        let y = texel_coordinate_f32(subtexel.resolve(uv[1] * self.height as f32 - 0.5))?;
        let (ix, iy) = (x.floor() as i64, y.floor() as i64);
        let (tx, ty) = (x - ix as f32, y - iy as f32);
        let mut result = [0.; 4];
        for dy in 0..2 {
            for dx in 0..2 {
                let xx = address[0].index(ix + dx, self.width as u32);
                let yy = address[1].index(iy + dy, self.height as u32);
                let weight = (if dx == 1 { tx } else { 1. - tx }) * (if dy == 1 { ty } else { 1. - ty });
                for (c, out) in result.iter_mut().enumerate() {
                    *out += self.at(xx, yy, 0)[c] * weight;
                }
            }
        }
        Ok(result)
    }

    /// The texel `uv` falls in (texel `i` covering `[i, i + 1)` after scaling by the size), addressed, in f32: point
    /// filtering. A coordinate that isn't finite or past an i32's range is refused.
    pub fn point(&self, uv: [f32; 2], address: [Address; 2]) -> Result<[f32; 4]> {
        self.plane()?;
        let x = texel_coordinate_f32(uv[0] * self.width as f32)?;
        let y = texel_coordinate_f32(uv[1] * self.height as f32)?;
        Ok(self.at(
            address[0].index(x.floor() as i64, self.width as u32),
            address[1].index(y.floor() as i64, self.height as u32),
            0,
        ))
    }

    /// The texel of a volume (or a 2D level, one slice) that `uvw` falls in, addressed along each axis, in f32.
    pub fn point_3d(&self, uvw: [f32; 3], address: [Address; 3]) -> Result<[f32; 4]> {
        let size = [self.width, self.height, self.depth];
        let mut index = [0usize; 3];
        for axis in 0..3 {
            let x = texel_coordinate_f32(uvw[axis] * size[axis] as f32)?;
            index[axis] = address[axis].index(x.floor() as i64, size[axis] as u32);
        }
        Ok(self.at(index[0], index[1], index[2]))
    }

    /// Linear between the eight texels about `uvw` in a volume, in f32: the coordinate per axis (scaled by the size,
    /// minus a half, resolved to `subtexel` precision), the weights `(1 - t)` and `t` per axis multiplied x, then y,
    /// then z, and the sum over the texels slice by slice, row by row. A coordinate that isn't finite or past an i32's
    /// range is refused.
    pub fn linear_3d(&self, uvw: [f32; 3], address: [Address; 3], subtexel: Subtexel) -> Result<[f32; 4]> {
        let size = [self.width, self.height, self.depth];
        let mut base = [0i64; 3];
        let mut t = [0f32; 3];
        for axis in 0..3 {
            let x = texel_coordinate_f32(subtexel.resolve(uvw[axis] * size[axis] as f32 - 0.5))?;
            base[axis] = x.floor() as i64;
            t[axis] = x - base[axis] as f32;
        }
        let weight = |d: i64, t: f32| if d == 1 { t } else { 1. - t };
        let mut result = [0.; 4];
        for dz in 0..2 {
            for dy in 0..2 {
                for dx in 0..2 {
                    let texel = self.at(
                        address[0].index(base[0] + dx, self.width as u32),
                        address[1].index(base[1] + dy, self.height as u32),
                        address[2].index(base[2] + dz, self.depth as u32),
                    );
                    let w = weight(dx, t[0]) * weight(dy, t[1]) * weight(dz, t[2]);
                    for (c, out) in result.iter_mut().enumerate() {
                        *out += texel[c] * w;
                    }
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    /// Texel indices out of range, worked by hand for four texels: repeat, mirrored repeat (-1 reads 0, 4 reads 3, the
    /// pattern 0 1 2 3 3 2 1 0 repeating), clamp, and mirrored once about the origin then the edge.
    #[test]
    fn addressing_out_of_range() {
        let read =
            |a: Address, from: i64, to: i64| -> Vec<usize> { (from..=to).map(|i| a.index(i, 4)).collect() };
        assert_eq!(read(Address::Wrap, -5, 5), [3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1]);
        assert_eq!(
            read(Address::Mirror, -5, 8),
            [3, 3, 2, 1, 0, 0, 1, 2, 3, 3, 2, 1, 0, 0]
        );
        assert_eq!(read(Address::Clamp, -2, 5), [0, 0, 0, 1, 2, 3, 3, 3]);
        assert_eq!(
            read(Address::MirrorOnce, -5, 5),
            [3, 3, 2, 1, 0, 0, 1, 2, 3, 3, 3]
        );
    }

    /// Direct3D 11's sub-texel precision truncates the texel coordinate to 1/256: a sample 0.3 of the way from black to
    /// white weighs white 76/256 (0.3 * 256 = 76.8, truncated), where the exact filter weighs it 0.3; on a 1/256 step
    /// both agree.
    #[test]
    fn d3d11_subtexel_precision_truncates_to_eight_bits() {
        let d = Levels::one(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255])
            .unwrap()
            .decoded(0, Transfer::Stored)
            .unwrap();
        let clamp = [Address::Clamp; 2];
        // texel coordinate 0.3: u = (0.3 + 0.5) / 2
        let u = 0.4f32;
        let exact = d.bilinear_at([u, 0.5], clamp, Subtexel::Exact).unwrap()[0];
        let d3d = d.bilinear_at([u, 0.5], clamp, Subtexel::D3d11).unwrap()[0];
        assert!((exact - 0.3).abs() < 1e-6, "{exact}");
        assert_eq!(d3d, 76. / 256.);
        let on_step = (64. / 256. + 0.5) / 2.;
        assert_eq!(
            d.bilinear_at([on_step, 0.5], clamp, Subtexel::D3d11).unwrap(),
            d.bilinear_at([on_step, 0.5], clamp, Subtexel::Exact).unwrap()
        );
    }

    /// A 2 x 1 texture, black then white: halfway between the texel centres is half white; at the left texel's centre
    /// black; wrapped, the left edge blends the right texel in, clamped it doesn't; sRGB colour decoded before
    /// filtering (a half of white's linear 1), alpha as stored.
    #[test]
    fn bilinear_between_texel_centres() {
        let l = Levels::one(2, 1, vec![0, 0, 0, 0, 255, 255, 255, 255]).unwrap();
        let wrap = [Address::Wrap; 2];
        let clamp = [Address::Clamp; 2];
        assert_eq!(
            l.bilinear(0, [0.5, 0.5], wrap, Transfer::Stored).unwrap(),
            [0.5; 4]
        );
        assert_eq!(
            l.bilinear(0, [0.25, 0.5], wrap, Transfer::Stored).unwrap(),
            [0.0; 4]
        );
        assert_eq!(
            l.bilinear(0, [0.0, 0.5], wrap, Transfer::Stored).unwrap(),
            [0.5; 4]
        );
        assert_eq!(
            l.bilinear(0, [0.0, 0.5], clamp, Transfer::Stored).unwrap(),
            [0.0; 4]
        );
        let d = l.decoded(0, Transfer::SrgbColour).unwrap();
        let half = d.bilinear([0.5, 0.5], wrap).unwrap();
        assert!(half.iter().all(|c| (c - 0.5).abs() < 1e-6), "{half:?}");
        // a 50% grey texel: its sRGB colour decodes to linear 0.2158...; its alpha stays 128/255
        let g = Levels::one(1, 1, vec![128, 128, 128, 128]).unwrap();
        let c = g.bilinear(0, [0.5, 0.5], wrap, Transfer::SrgbColour).unwrap();
        assert!(
            (c[0] - crate::color::srgb_to_linear(128.0 / 255.0)).abs() < 1e-15
                && (c[3] - 128.0 / 255.0).abs() < 1e-15
        );
    }

    /// A footprint of one base texel is level 0, of four texels level 2 (the longer derivative counts, in texels of
    /// each axis's size); trilinear between levels 0 (black) and 1 (white) gives the fraction, and clamps to the
    /// stored levels.
    #[test]
    fn trilinear_at_a_footprint() {
        assert_eq!(
            footprint_lod(8, 4, [[1.0 / 8.0, 0.0], [0.0, 1.0 / 4.0]], 0.0),
            0.0
        );
        assert_eq!(footprint_lod(8, 4, [[0.0, 0.0], [0.0, 1.0]], 0.0), 2.0);
        assert_eq!(footprint_lod(8, 4, [[0.0; 2]; 2], 3.0), f64::NEG_INFINITY);
        let l = Levels {
            width: 2,
            height: 2,
            levels: vec![vec![0; 16], vec![255; 4]],
        };
        let wrap = [Address::Wrap; 2];
        let at = |lod| l.trilinear([0.5, 0.5], lod, wrap, Transfer::Stored).unwrap()[0];
        assert_eq!((at(-1.0), at(0.25), at(1.0), at(7.0)), (0.0, 0.25, 1.0, 1.0));
        assert!(l.trilinear([0.5, 0.5], f64::NAN, wrap, Transfer::Stored).is_err());
    }

    /// Point filtering takes the texel a coordinate falls in, addressed, its transfer applied; the nearest level
    /// rounds a half up and clamps to the stored levels; reversed rows put the bottom row at v = 0 in every level.
    #[test]
    fn point_filtering_nearest_levels_and_reversed_rows() {
        // 2 x 2, rows top first: black, white / grey, white; then a 1 x 1 level
        let l = Levels {
            width: 2,
            height: 2,
            levels: vec![[[0; 4], [255; 4], [128; 4], [255; 4]].concat(), vec![64; 4]],
        };
        let (wrap, clamp) = ([Address::Wrap; 2], [Address::Clamp; 2]);
        let at = |l: &Levels, uv: [f64; 2], a| l.point(0, uv, a, Transfer::Stored).unwrap()[0];
        assert_eq!(at(&l, [0.25, 0.25], wrap), 0.0);
        assert_eq!(at(&l, [0.5, 0.25], wrap), 1.0);
        assert_eq!(at(&l, [0.25, 0.75], wrap), 128.0 / 255.0);
        // past the left edge: wrapped to the right texel, clamped to the left one
        assert_eq!(
            (at(&l, [-0.25, 0.25], wrap), at(&l, [-0.25, 0.25], clamp)),
            (1.0, 0.0)
        );
        let c = l.point(0, [0.25, 0.75], wrap, Transfer::SrgbColour).unwrap();
        assert_eq!(c[0], crate::color::srgb8_to_linear(128));
        assert!(l.point(0, [f64::NAN, 0.0], wrap, Transfer::Stored).is_err());
        assert_eq!(
            [-3.0, 0.49, 0.5, 1.2, 9.0].map(|lod| l.nearest_level(lod).unwrap()),
            [0, 0, 1, 1, 1]
        );
        assert!(l.nearest_level(f64::NAN).is_err());
        let up = l.clone().rows_reversed().unwrap();
        assert_eq!(at(&up, [0.25, 0.25], wrap), 128.0 / 255.0);
        assert_eq!(at(&up, [0.25, 0.75], wrap), 0.0);
        assert_eq!(up.levels[1], vec![64; 4]);
        let short = Levels {
            width: 2,
            height: 2,
            levels: vec![vec![0; 12]],
        };
        assert!(short.rows_reversed().is_err());
    }

    /// Float texels as held: a 2 x 2 level point-sampled (the texel a coordinate falls in, addressed) and bilinear
    /// (the middle is the mean of the four); a count other than the size's, or no texel, is refused; a 2D sample of a
    /// volume is refused.
    #[test]
    fn float_texels_point_and_bilinear() {
        let red_green = vec![
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
        ];
        let d = Decoded::from_texels(2, 2, red_green.clone()).unwrap();
        let (wrap, clamp) = ([Address::Wrap; 2], [Address::Clamp; 2]);
        assert_eq!(d.point([0.25, 0.25], clamp).unwrap(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.point([0.75, 0.25], clamp).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.point([0.25, 0.75], clamp).unwrap(), [0.0, 1.0, 0.0, 1.0]);
        // past the edges: clamped to the edge texel, wrapped round
        assert_eq!(d.point([1.75, -3.0], clamp).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.point([1.25, 0.25], wrap).unwrap(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.point([-0.25, 0.25], wrap).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.bilinear([0.5, 0.5], clamp).unwrap(), [0.5, 0.5, 0.0, 1.0]);
        assert_eq!(d.bilinear([0.375, 0.25], clamp).unwrap(), [0.25, 0.0, 0.0, 1.0]);
        // values past 0..1 are filtered as they are
        let hdr = Decoded::from_texels(2, 1, vec![[-2.0; 4], [6.0; 4]]).unwrap();
        assert_eq!(hdr.bilinear([0.5, 0.5], clamp).unwrap(), [2.0; 4]);
        assert!(Decoded::from_texels(2, 2, red_green[..3].to_vec()).is_err());
        assert!(Decoded::from_texels(0, 2, Vec::new()).is_err());
        assert!(d.point([f32::NAN, 0.5], clamp).is_err());
        let volume = Decoded::volume(1, 1, 2, vec![[0.0; 4], [1.0; 4]]).unwrap();
        assert!(volume.point([0.5, 0.5], clamp).is_err());
        assert!(volume.bilinear([0.5, 0.5], clamp).is_err());
    }

    /// A 2 x 2 x 2 volume whose texel (x, y, z) is (x, y, z, 1): point sampling takes the texel a coordinate falls in;
    /// linear filtering between the eight texels gives the coordinate's position between their centres, clamped at the
    /// edges and wrapped round; the texel a load reads is the one at its index, none past the size.
    #[test]
    fn volumes_point_linear_and_texels() {
        let texels: Vec<[f32; 4]> = (0..8)
            .map(|i| [(i % 2) as f32, (i / 2 % 2) as f32, (i / 4) as f32, 1.0])
            .collect();
        let v = Decoded::volume(2, 2, 2, texels).unwrap();
        assert_eq!(v.depth(), 2);
        let (wrap, clamp) = ([Address::Wrap; 3], [Address::Clamp; 3]);
        assert_eq!(
            v.point_3d([0.75, 0.25, 0.75], clamp).unwrap(),
            [1.0, 0.0, 1.0, 1.0]
        );
        assert_eq!(
            v.point_3d([0.25, 1.25, -0.25], wrap).unwrap(),
            [0.0, 0.0, 1.0, 1.0]
        );
        // halfway between the centres on every axis, a quarter of the way along x, at a corner's centre
        let exact = Subtexel::Exact;
        assert_eq!(v.linear_3d([0.5; 3], clamp, exact).unwrap(), [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(
            v.linear_3d([0.375, 0.25, 0.25], clamp, exact).unwrap(),
            [0.25, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            v.linear_3d([0.25, 0.25, 0.75], clamp, exact).unwrap(),
            [0.0, 0.0, 1.0, 1.0]
        );
        // at the left edge: clamped it is the edge texel, wrapped it blends the far one in
        assert_eq!(v.linear_3d([0.0, 0.25, 0.25], clamp, exact).unwrap()[0], 0.0);
        assert_eq!(v.linear_3d([0.0, 0.25, 0.25], wrap, exact).unwrap()[0], 0.5);
        assert!(v.linear_3d([0.5, f32::INFINITY, 0.5], clamp, exact).is_err());
        assert_eq!(v.texel(1, 0, 1), Some([1.0, 0.0, 1.0, 1.0]));
        assert_eq!(v.texel(2, 0, 0), None);
        assert_eq!(v.texel(0, -1, 0), None);
        assert_eq!(v.texel(0, 0, 2), None);
        // a 2D level is one slice
        let d = Decoded::from_texels(1, 1, vec![[3.0; 4]]).unwrap();
        assert_eq!(d.point_3d([0.5; 3], clamp).unwrap(), [3.0; 4]);
        assert_eq!(d.texel(0, 0, 0), Some([3.0; 4]));
        assert_eq!(d.texel(0, 0, 1), None);
    }

    /// A coordinate that isn't finite, or whose texel is past an i32's range, is refused, never sampled (NaN texels)
    /// and never an overflow: in both precisions.
    #[test]
    fn nonfinite_or_excessive_coordinates_are_refused() {
        let l = Levels::one(2, 2, vec![10; 16]).unwrap();
        let wrap = [Address::Wrap; 2];
        for uv in [[f64::NAN, 0.5], [0.5, f64::INFINITY], [1e300, 0.5], [0.5, -1e12]] {
            assert!(l.bilinear(0, uv, wrap, Transfer::Stored).is_err(), "{uv:?}");
            assert!(l.trilinear(uv, 0.0, wrap, Transfer::Stored).is_err(), "{uv:?}");
            assert!(
                l.decoded(0, Transfer::Stored)
                    .unwrap()
                    .bilinear(uv.map(|v| v as f32), wrap)
                    .is_err(),
                "{uv:?}"
            );
        }
        assert!(l.trilinear([1e8, -1e8], 0.0, wrap, Transfer::Stored).is_ok());
    }
}
