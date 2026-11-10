//! Bitmap view decoding (ANL-15): interprets bytes at an offset as pixels
//! of a given width, pixel format and row stride and returns RGBA.

use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::invalid;

/// Pixel formats of the bitmap view. Multi-byte names give the byte order
/// in memory (`Bgra8888` = B, G, R, A); 16-bit formats are read as integers
/// in the configured endianness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PixelFormat {
    Rgb565,
    Rgb555,
    Rgb888,
    Bgr888,
    Rgba8888,
    Bgra8888,
    Argb8888,
    Gray1,
    Gray2,
    Gray4,
    Gray8,
    /// 8-bit indices into a palette read from another offset.
    Indexed8,
}

impl PixelFormat {
    /// Bits per pixel.
    pub fn bits(self) -> u32 {
        match self {
            Self::Rgb565 | Self::Rgb555 => 16,
            Self::Rgb888 | Self::Bgr888 => 24,
            Self::Rgba8888 | Self::Bgra8888 | Self::Argb8888 => 32,
            Self::Gray1 => 1,
            Self::Gray2 => 2,
            Self::Gray4 => 4,
            Self::Gray8 | Self::Indexed8 => 8,
        }
    }
}

/// Layout of palette entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaletteFormat {
    Rgb888,
    Bgr888,
    Rgba8888,
    /// B, G, R, A — with `ignore_alpha` this is BMP's `RGBQUAD`.
    Bgra8888,
}

impl PaletteFormat {
    fn entry_size(self) -> usize {
        match self {
            Self::Rgb888 | Self::Bgr888 => 3,
            Self::Rgba8888 | Self::Bgra8888 => 4,
        }
    }
}

/// Palette for [`PixelFormat::Indexed8`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Palette {
    pub offset: u64,
    pub format: PaletteFormat,
    /// Number of entries (1…256).
    pub entries: u16,
    /// Treat entries as opaque (BMP palettes store 0 in the fourth byte).
    pub ignore_alpha: bool,
}

/// Options of the bitmap view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BitmapOptions {
    pub offset: u64,
    pub width: u32,
    /// Rows to decode; `None` decodes as many rows as the data provides
    /// (up to [`MAX_DIMENSION`]).
    pub height: Option<u32>,
    /// Bytes per row; default `ceil(width * bits / 8)`.
    pub stride: Option<u64>,
    pub format: PixelFormat,
    /// Byte order of 16-bit pixels.
    pub big_endian: bool,
    /// Sub-byte pixels: least significant bits first (default MSB first).
    pub lsb_first: bool,
    /// Store rows bottom-up (as in BMP).
    pub flip_vertical: bool,
    pub palette: Option<Palette>,
}

/// Decoded image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BitmapImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes; pixels past the end of data are
    /// transparent.
    pub rgba: Vec<u8>,
    /// Stride actually used.
    pub stride: u64,
}

/// Largest width or height.
pub const MAX_DIMENSION: u32 = 16_384;
/// Largest pixel count of one decode.
pub const MAX_PIXELS: u64 = 1 << 26;

fn expand(v: u32, bits: u32) -> u8 {
    match bits {
        5 => ((v << 3) | (v >> 2)) as u8,
        6 => ((v << 2) | (v >> 4)) as u8,
        _ => v as u8,
    }
}

fn palette_rgba(bytes: &[u8], p: &Palette) -> Vec<[u8; 4]> {
    bytes
        .chunks_exact(p.format.entry_size())
        .take(usize::from(p.entries))
        .map(|e| {
            let mut c = match p.format {
                PaletteFormat::Rgb888 => [e[0], e[1], e[2], 255],
                PaletteFormat::Bgr888 => [e[2], e[1], e[0], 255],
                PaletteFormat::Rgba8888 => [e[0], e[1], e[2], e[3]],
                PaletteFormat::Bgra8888 => [e[2], e[1], e[0], e[3]],
            };
            if p.ignore_alpha {
                c[3] = 255;
            }
            c
        })
        .collect()
}

fn stride_of(opts: &BitmapOptions) -> u64 {
    opts.stride
        .unwrap_or_else(|| (u64::from(opts.width) * u64::from(opts.format.bits())).div_ceil(8))
}

fn validate(opts: &BitmapOptions) -> Result<u64> {
    if opts.width == 0 || opts.width > MAX_DIMENSION {
        return Err(invalid(format!("bitmap width must be 1…{MAX_DIMENSION}")));
    }
    if opts.height.is_some_and(|h| h > MAX_DIMENSION) {
        return Err(invalid(format!("bitmap height must be ≤ {MAX_DIMENSION}")));
    }
    let stride = stride_of(opts);
    let min = (u64::from(opts.width) * u64::from(opts.format.bits())).div_ceil(8);
    if stride < min {
        return Err(invalid(format!(
            "stride {stride} is shorter than a row ({min} bytes)"
        )));
    }
    if opts.format == PixelFormat::Indexed8 && opts.palette.is_none() {
        return Err(invalid("indexed pixels need a palette"));
    }
    if let Some(p) = &opts.palette
        && !(1..=256).contains(&p.entries)
    {
        return Err(invalid("palette must have 1…256 entries"));
    }
    Ok(stride)
}

fn decode_row(row: &[u8], out: &mut [u8], opts: &BitmapOptions, palette: &[[u8; 4]]) {
    let w = opts.width as usize;
    let rd16 = |p: &[u8]| {
        u32::from(if opts.big_endian {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        })
    };
    for x in 0..w {
        let px: Option<[u8; 4]> = match opts.format {
            PixelFormat::Rgb565 => row.get(x * 2..x * 2 + 2).map(|p| {
                let v = rd16(p);
                [
                    expand(v >> 11, 5),
                    expand((v >> 5) & 63, 6),
                    expand(v & 31, 5),
                    255,
                ]
            }),
            PixelFormat::Rgb555 => row.get(x * 2..x * 2 + 2).map(|p| {
                let v = rd16(p);
                [
                    expand((v >> 10) & 31, 5),
                    expand((v >> 5) & 31, 5),
                    expand(v & 31, 5),
                    255,
                ]
            }),
            PixelFormat::Rgb888 => row.get(x * 3..x * 3 + 3).map(|p| [p[0], p[1], p[2], 255]),
            PixelFormat::Bgr888 => row.get(x * 3..x * 3 + 3).map(|p| [p[2], p[1], p[0], 255]),
            PixelFormat::Rgba8888 => row.get(x * 4..x * 4 + 4).map(|p| [p[0], p[1], p[2], p[3]]),
            PixelFormat::Bgra8888 => row.get(x * 4..x * 4 + 4).map(|p| [p[2], p[1], p[0], p[3]]),
            PixelFormat::Argb8888 => row.get(x * 4..x * 4 + 4).map(|p| [p[1], p[2], p[3], p[0]]),
            PixelFormat::Gray8 => row.get(x).map(|&g| [g, g, g, 255]),
            PixelFormat::Indexed8 => row
                .get(x)
                .map(|&i| palette.get(usize::from(i)).copied().unwrap_or([0, 0, 0, 0])),
            PixelFormat::Gray1 | PixelFormat::Gray2 | PixelFormat::Gray4 => {
                let bits = opts.format.bits() as usize;
                let per = 8 / bits;
                row.get(x / per).map(|&b| {
                    let k = x % per;
                    let shift = if opts.lsb_first {
                        k * bits
                    } else {
                        8 - bits - k * bits
                    };
                    let v = u32::from(b >> shift) & ((1 << bits) - 1);
                    let g = (v * 255 / ((1 << bits) - 1)) as u8;
                    [g, g, g, 255]
                })
            }
        };
        if let Some(px) = px {
            out[x * 4..x * 4 + 4].copy_from_slice(&px);
        }
    }
}

/// Decodes pixels from a buffer (`data` starts at the first row; the
/// palette, if any, is passed separately).
pub fn decode_bitmap_bytes(
    data: &[u8],
    palette: Option<&[u8]>,
    opts: &BitmapOptions,
) -> Result<BitmapImage> {
    let stride = validate(opts)?;
    let available = (data.len() as u64).div_ceil(stride.max(1));
    let max_rows = (MAX_PIXELS / u64::from(opts.width)).min(u64::from(MAX_DIMENSION));
    let height = opts
        .height
        .map_or(available.min(max_rows), u64::from)
        .min(max_rows) as u32;
    let pal = match (&opts.palette, palette) {
        (Some(p), Some(bytes)) => palette_rgba(bytes, p),
        _ => Vec::new(),
    };
    let row_px = opts.width as usize * 4;
    let mut rgba = vec![0u8; row_px * height as usize];
    for y in 0..height as usize {
        let begin = (y as u64 * stride).min(data.len() as u64) as usize;
        let end = (begin as u64 + stride).min(data.len() as u64) as usize;
        let dst_y = if opts.flip_vertical {
            height as usize - 1 - y
        } else {
            y
        };
        decode_row(
            &data[begin..end],
            &mut rgba[dst_y * row_px..(dst_y + 1) * row_px],
            opts,
            &pal,
        );
    }
    Ok(BitmapImage {
        width: opts.width,
        height,
        rgba,
        stride,
    })
}

/// Decodes the bitmap view of a snapshot (ANL-15).
pub fn decode_bitmap(tree: &Tree, opts: &BitmapOptions, cancel: &Cancel) -> Result<BitmapImage> {
    let stride = validate(opts)?;
    cancel.check()?;
    let max_rows = (MAX_PIXELS / u64::from(opts.width)).min(u64::from(MAX_DIMENSION));
    let rows = opts.height.map_or(max_rows, u64::from).min(max_rows);
    let want = rows.saturating_mul(stride);
    if want > MAX_PIXELS * 4 {
        return Err(Error::TooLarge {
            len: want,
            limit: MAX_PIXELS * 4,
        });
    }
    let data = tree.read_vec(opts.offset, want as usize)?;
    cancel.check()?;
    let palette = match &opts.palette {
        Some(p) => Some(tree.read_vec(p.offset, usize::from(p.entries) * p.format.entry_size())?),
        None => None,
    };
    decode_bitmap_bytes(&data, palette.as_deref(), opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::tree;

    fn opts(format: PixelFormat, width: u32) -> BitmapOptions {
        BitmapOptions {
            offset: 0,
            width,
            height: None,
            stride: None,
            format,
            big_endian: false,
            lsb_first: false,
            flip_vertical: false,
            palette: None,
        }
    }

    fn px(img: &BitmapImage, i: usize) -> [u8; 4] {
        img.rgba[i * 4..i * 4 + 4].try_into().unwrap()
    }

    #[test]
    fn rgb_formats() {
        let red565 = 0xF800u16.to_le_bytes();
        let img = decode_bitmap_bytes(&red565, None, &opts(PixelFormat::Rgb565, 1)).unwrap();
        assert_eq!(px(&img, 0), [255, 0, 0, 255]);
        let mut o = opts(PixelFormat::Rgb565, 1);
        o.big_endian = true;
        let img = decode_bitmap_bytes(&0x07E0u16.to_be_bytes(), None, &o).unwrap();
        assert_eq!(px(&img, 0), [0, 255, 0, 255]);
        let img = decode_bitmap_bytes(
            &0x001Fu16.to_le_bytes(),
            None,
            &opts(PixelFormat::Rgb555, 1),
        )
        .unwrap();
        assert_eq!(px(&img, 0), [0, 0, 255, 255]);
        let d = [1, 2, 3, 4];
        let one = |f| px(&decode_bitmap_bytes(&d, None, &opts(f, 1)).unwrap(), 0);
        assert_eq!(one(PixelFormat::Rgb888), [1, 2, 3, 255]);
        assert_eq!(one(PixelFormat::Bgr888), [3, 2, 1, 255]);
        assert_eq!(one(PixelFormat::Rgba8888), [1, 2, 3, 4]);
        assert_eq!(one(PixelFormat::Bgra8888), [3, 2, 1, 4]);
        assert_eq!(one(PixelFormat::Argb8888), [2, 3, 4, 1]);
        assert_eq!(one(PixelFormat::Gray8), [1, 1, 1, 255]);
    }

    #[test]
    fn packed_gray() {
        let img = decode_bitmap_bytes(&[0b1010_0000], None, &opts(PixelFormat::Gray1, 4)).unwrap();
        let g: Vec<u8> = (0..4).map(|i| px(&img, i)[0]).collect();
        assert_eq!(g, [255, 0, 255, 0]);
        let mut o = opts(PixelFormat::Gray2, 4);
        let img = decode_bitmap_bytes(&[0b00_01_10_11], None, &o).unwrap();
        let g: Vec<u8> = (0..4).map(|i| px(&img, i)[0]).collect();
        assert_eq!(g, [0, 85, 170, 255]);
        o.lsb_first = true;
        let img = decode_bitmap_bytes(&[0b00_01_10_11], None, &o).unwrap();
        assert_eq!(px(&img, 0)[0], 255);
        let img = decode_bitmap_bytes(&[0x0F], None, &opts(PixelFormat::Gray4, 2)).unwrap();
        assert_eq!((px(&img, 0)[0], px(&img, 1)[0]), (0, 255));
    }

    #[test]
    fn stride_height_flip_and_partial_rows() {
        // 2×2 gray with 3-byte stride (one padding byte per row), 1 row short.
        let d = [10, 20, 99, 30];
        let mut o = opts(PixelFormat::Gray8, 2);
        o.stride = Some(3);
        let img = decode_bitmap_bytes(&d, None, &o).unwrap();
        assert_eq!(img.height, 2);
        assert_eq!(px(&img, 2), [30, 30, 30, 255]);
        assert_eq!(px(&img, 3), [0, 0, 0, 0]);
        o.flip_vertical = true;
        let img = decode_bitmap_bytes(&d, None, &o).unwrap();
        assert_eq!(px(&img, 0), [30, 30, 30, 255]);
        o.stride = Some(1);
        assert!(decode_bitmap_bytes(&d, None, &o).is_err());
        assert!(decode_bitmap_bytes(&d, None, &opts(PixelFormat::Gray8, 0)).is_err());
    }

    #[test]
    fn indexed_from_tree() {
        // Palette (BGRX like BMP) at 0, pixels at 8.
        let mut d = vec![0, 0, 255, 0, 0, 255, 0, 0];
        d.extend([1, 0, 7]);
        let o = BitmapOptions {
            offset: 8,
            height: Some(1),
            palette: Some(Palette {
                offset: 0,
                format: PaletteFormat::Bgra8888,
                entries: 2,
                ignore_alpha: true,
            }),
            ..opts(PixelFormat::Indexed8, 3)
        };
        let img = decode_bitmap(&tree(&d), &o, &Cancel::new()).unwrap();
        assert_eq!(px(&img, 0), [0, 255, 0, 255]);
        assert_eq!(px(&img, 1), [255, 0, 0, 255]);
        assert_eq!(px(&img, 2), [0, 0, 0, 0]);
        let no_pal = opts(PixelFormat::Indexed8, 3);
        assert!(decode_bitmap(&tree(&d), &no_pal, &Cancel::new()).is_err());
    }
}
