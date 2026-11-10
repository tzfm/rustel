//! Decoded pictures, borrowed from their decoders, as RGBA thumbnails.
//!
//! A thumbnail averages the source samples under each of its pixels and
//! converts only those few pixels from YUV. A 4K frame costs one pass over
//! its samples and no full-size RGBA copy, and a frame that is not kept
//! costs nothing here at all.

/// One plane's samples, borrowed from the decoder that wrote them.
#[derive(Clone, Copy)]
pub(super) enum Samples<'a> {
    /// One byte a sample.
    Narrow(&'a [u8]),
    /// One `u16` a sample.
    Wide(&'a [u16]),
    /// Two little-endian bytes a sample.
    WideBytes(&'a [u8]),
}

impl Samples<'_> {
    fn len(self) -> usize {
        match self {
            Self::Narrow(data) => data.len(),
            Self::Wide(data) => data.len(),
            Self::WideBytes(data) => data.len() / 2,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Plane<'a> {
    pub(super) samples: Samples<'a>,
    /// Samples from the start of one row to the start of the next.
    pub(super) stride: usize,
    pub(super) width: usize,
    pub(super) height: usize,
}

/// The YUV to RGB matrices pictures signal.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl Matrix {
    /// The luma weights of red and blue.
    fn weights(self) -> (f32, f32) {
        match self {
            Self::Bt601 => (0.299, 0.114),
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// How a picture's YUV maps to RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Colour {
    matrix: Matrix,
    full_range: bool,
}

impl Colour {
    /// The limited-range BT.601 that VP8 and unsignalled SD video use.
    pub(super) const BT601: Self = Self {
        matrix: Matrix::Bt601,
        full_range: false,
    };

    /// The colour a stream signals: `matrix` is an ITU-T H.273
    /// `matrix_coefficients` code, as H.264, HEVC and AV1 all carry it. An
    /// unspecified code follows the picture's height, as players do:
    /// BT.709 from 720 rows up, BT.601 below.
    pub(super) fn signalled(matrix: u8, full_range: bool, height: u32) -> Self {
        let matrix = match matrix {
            1 => Matrix::Bt709,
            5 | 6 => Matrix::Bt601,
            9 | 10 => Matrix::Bt2020,
            _ if height >= 720 => Matrix::Bt709,
            _ => Matrix::Bt601,
        };
        Self { matrix, full_range }
    }

    /// One pixel from 8-bit Y, Cb and Cr.
    fn rgb(self, luma: f32, blue: f32, red: f32) -> [u8; 3] {
        let (luma, blue, red) = if self.full_range {
            (luma / 255.0, (blue - 128.0) / 255.0, (red - 128.0) / 255.0)
        } else {
            (
                (luma - 16.0) / 219.0,
                (blue - 128.0) / 224.0,
                (red - 128.0) / 224.0,
            )
        };
        let (kr, kb) = self.matrix.weights();
        let r = luma + 2.0 * (1.0 - kr) * red;
        let b = luma + 2.0 * (1.0 - kb) * blue;
        let g = (luma - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b].map(|channel| (channel * 255.0).round().clamp(0.0, 255.0) as u8)
    }
}

pub(super) struct Yuv<'a> {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) luma: Plane<'a>,
    /// Cb then Cr. A monochrome picture has none, and its chroma is
    /// neutral.
    pub(super) chroma: Option<[Plane<'a>; 2]>,
    /// Significant bits a sample: 8 or more.
    pub(super) depth: u8,
    pub(super) colour: Colour,
}

pub(super) enum Picture<'a> {
    Yuv(Yuv<'a>),
    /// Packed RGBA, as Motion JPEG decodes.
    Rgba {
        width: u32,
        height: u32,
        pixels: &'a [u8],
    },
}

impl Picture<'_> {
    pub(super) fn size(&self) -> (u32, u32) {
        match self {
            Self::Yuv(yuv) => (yuv.width, yuv.height),
            Self::Rgba { width, height, .. } => (*width, *height),
        }
    }

    /// The picture averaged into `width` by `height` RGBA pixels.
    pub(super) fn thumbnail(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let (columns, rows) = (width as usize, height as usize);
        match self {
            Self::Rgba {
                width: source_width,
                height: source_height,
                pixels,
            } => {
                let needed = *source_width as usize * *source_height as usize * 4;
                if pixels.len() < needed {
                    return Err("the decoded picture is short".to_owned());
                }
                Ok(scale_rgba(
                    pixels,
                    *source_width,
                    *source_height,
                    width,
                    height,
                ))
            }
            Self::Yuv(yuv) => {
                let luma = cell_means(yuv.luma, columns, rows)?;
                let chroma = match yuv.chroma {
                    Some([blue, red]) => Some((
                        cell_means(blue, columns, rows)?,
                        cell_means(red, columns, rows)?,
                    )),
                    None => None,
                };
                let scale = f32::from(1u16 << (yuv.depth.clamp(8, 16) - 8));
                let mut rgba = Vec::with_capacity(columns * rows * 4);
                for (at, &luma) in luma.iter().enumerate() {
                    let (blue, red) = chroma.as_ref().map_or((128.0, 128.0), |(blue, red)| {
                        (f32::from(blue[at]) / scale, f32::from(red[at]) / scale)
                    });
                    rgba.extend_from_slice(&yuv.colour.rgb(f32::from(luma) / scale, blue, red));
                    rgba.push(255);
                }
                Ok(rgba)
            }
        }
    }
}

/// The mean sample under each of `columns` by `rows` cells laid over the
/// plane. Every cell covers at least one sample.
fn cell_means(plane: Plane<'_>, columns: usize, rows: usize) -> Result<Vec<u16>, String> {
    let Plane {
        samples,
        stride,
        width,
        height,
    } = plane;
    let fits = width > 0
        && height > 0
        && stride >= width
        && (height - 1)
            .checked_mul(stride)
            .and_then(|start| start.checked_add(width))
            .is_some_and(|needed| needed <= samples.len());
    if !fits {
        return Err("a decoded plane is shorter than its size".to_owned());
    }
    Ok(match samples {
        Samples::Narrow(data) => means(plane, columns, rows, |at| u32::from(data[at])),
        Samples::Wide(data) => means(plane, columns, rows, |at| u32::from(data[at])),
        Samples::WideBytes(data) => means(plane, columns, rows, |at| {
            u32::from(u16::from_le_bytes([data[2 * at], data[2 * at + 1]]))
        }),
    })
}

fn means(plane: Plane<'_>, columns: usize, rows: usize, read: impl Fn(usize) -> u32) -> Vec<u16> {
    let mut out = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        let top = row * plane.height / rows;
        let bottom = ((row + 1) * plane.height / rows).max(top + 1);
        for column in 0..columns {
            let left = column * plane.width / columns;
            let right = ((column + 1) * plane.width / columns).max(left + 1);
            let mut sum = 0u64;
            for y in top..bottom {
                let start = y * plane.stride;
                for x in left..right {
                    sum += u64::from(read(start + x));
                }
            }
            let count = ((bottom - top) * (right - left)) as u64;
            out.push(u16::try_from(sum / count).unwrap_or(u16::MAX));
        }
    }
    out
}

/// Average the source pixels under each destination pixel. Nearest sampling
/// would shimmer as the clip plays. A box keeps still areas still.
pub(super) fn scale_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let (sw, sh, dw, dh) = (
        source_width as usize,
        source_height as usize,
        width as usize,
        height as usize,
    );
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(dw * dh * 4);
    for dy in 0..dh {
        // Source rows this destination row covers.
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1);
            let (mut red, mut green, mut blue) = (0u64, 0u64, 0u64);
            let mut pixels = 0u64;
            for y in y0..y1.min(sh) {
                let row = y * sw;
                for x in x0..x1.min(sw) {
                    let at = (row + x) * 4;
                    red += u64::from(source[at]);
                    green += u64::from(source[at + 1]);
                    blue += u64::from(source[at + 2]);
                    pixels += 1;
                }
            }
            if pixels == 0 {
                out.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                let mean = |sum: u64| u8::try_from(sum / pixels).unwrap_or(u8::MAX);
                out.push(mean(red));
                out.push(mean(green));
                out.push(mean(blue));
                out.push(255);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(samples: Samples<'_>, width: usize, height: usize) -> Plane<'_> {
        Plane {
            samples,
            stride: width,
            width,
            height,
        }
    }

    #[test]
    fn a_huge_frame_is_averaged_not_sampled() {
        // A red pixel and a blue one under one destination pixel must
        // average, not pick a side.
        let source = [255, 0, 0, 255, 0, 0, 255, 255];
        let scaled = scale_rgba(&source, 2, 1, 1, 1);
        assert_eq!(scaled, vec![127, 0, 127, 255]);
    }

    #[test]
    fn limited_range_black_and_white_reach_the_ends() {
        for colour in [
            Colour::BT601,
            Colour::signalled(1, false, 1080),
            Colour::signalled(9, false, 2160),
        ] {
            assert_eq!(colour.rgb(16.0, 128.0, 128.0), [0, 0, 0]);
            assert_eq!(colour.rgb(235.0, 128.0, 128.0), [255, 255, 255]);
        }
        let full = Colour::signalled(1, true, 1080);
        assert_eq!(full.rgb(0.0, 128.0, 128.0), [0, 0, 0]);
        assert_eq!(full.rgb(255.0, 128.0, 128.0), [255, 255, 255]);
    }

    /// The signalled matrix wins over the picture's height, so a clip
    /// shows the colour it was graded in.
    #[test]
    fn the_signalled_matrix_decides_the_colour() {
        let bt601 = Colour::signalled(6, false, 1080).rgb(126.0, 128.0, 160.0);
        let bt709 = Colour::signalled(1, false, 480).rgb(126.0, 128.0, 160.0);
        assert!(bt601[0] < bt709[0], "{bt601:?} against {bt709:?}");
        assert_eq!(
            Colour::signalled(2, false, 720),
            Colour::signalled(1, false, 720),
            "unspecified HD is BT.709"
        );
        assert_eq!(
            Colour::signalled(2, false, 480),
            Colour::BT601,
            "unspecified SD is BT.601"
        );
    }

    #[test]
    fn a_ten_bit_picture_thumbnails_like_its_eight_bit_twin() {
        let narrow = [235u8; 4];
        let neutral = [128u8; 1];
        let eight = Picture::Yuv(Yuv {
            width: 2,
            height: 2,
            luma: plane(Samples::Narrow(&narrow), 2, 2),
            chroma: Some([
                plane(Samples::Narrow(&neutral), 1, 1),
                plane(Samples::Narrow(&neutral), 1, 1),
            ]),
            depth: 8,
            colour: Colour::BT601,
        });
        let wide = [940u16; 4];
        let wide_neutral = [512u16; 1];
        let ten = Picture::Yuv(Yuv {
            width: 2,
            height: 2,
            luma: plane(Samples::Wide(&wide), 2, 2),
            chroma: Some([
                plane(Samples::Wide(&wide_neutral), 1, 1),
                plane(Samples::Wide(&wide_neutral), 1, 1),
            ]),
            depth: 10,
            colour: Colour::BT601,
        });
        let bytes: Vec<u8> = wide
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let neutral_bytes = 512u16.to_le_bytes();
        let ten_bytes = Picture::Yuv(Yuv {
            width: 2,
            height: 2,
            luma: plane(Samples::WideBytes(&bytes), 2, 2),
            chroma: Some([
                plane(Samples::WideBytes(&neutral_bytes), 1, 1),
                plane(Samples::WideBytes(&neutral_bytes), 1, 1),
            ]),
            depth: 10,
            colour: Colour::BT601,
        });
        let white = [255, 255, 255, 255];
        assert_eq!(eight.thumbnail(1, 1).expect("fits"), white);
        assert_eq!(ten.thumbnail(1, 1).expect("fits"), white);
        assert_eq!(ten_bytes.thumbnail(1, 1).expect("fits"), white);
    }

    #[test]
    fn a_monochrome_picture_is_grey() {
        let luma = [126u8; 4];
        let picture = Picture::Yuv(Yuv {
            width: 2,
            height: 2,
            luma: plane(Samples::Narrow(&luma), 2, 2),
            chroma: None,
            depth: 8,
            colour: Colour::BT601,
        });
        let pixel = picture.thumbnail(1, 1).expect("fits");
        assert_eq!(pixel[0], pixel[1]);
        assert_eq!(pixel[1], pixel[2]);
    }

    #[test]
    fn a_plane_shorter_than_its_size_is_refused() {
        let luma = [16u8; 3];
        let picture = Picture::Yuv(Yuv {
            width: 2,
            height: 2,
            luma: plane(Samples::Narrow(&luma), 2, 2),
            chroma: None,
            depth: 8,
            colour: Colour::BT601,
        });
        assert!(picture.thumbnail(1, 1).is_err());
    }
}
