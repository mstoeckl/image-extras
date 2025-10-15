//! Decoding of PSD Images
//!
//! .psd images are the working format of the Photoshop raster editor.
//!
//! The [PsdDecoder] only supports extracting the main composite image from a
//! .psd file, and does not expose layer details.
//!
//! # Related Links
//! * <https://en.wikipedia.org/wiki/Adobe_Photoshop#File_format> - Wikipedia page
//! * <https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/> - Partial specification

use core::error::Error;
use core::fmt::{Debug, Display};
use core::marker::PhantomData;

use std::io::{BufRead, Read};

use image::error::{
    DecodingError, ImageFormatHint, LimitError, LimitErrorKind, UnsupportedError,
    UnsupportedErrorKind,
};
use image::Limits;
use image::{ColorType, ImageDecoder, ImageError, ImageResult};
use zune_core::bit_depth::BitDepth as ZuneBitDepth;
use zune_core::colorspace::ColorSpace as ZuneColorSpace;
use zune_core::result::DecodingResult;
use zune_psd;

/// Decoder for PSD images.
pub struct PsdDecoder<R> {
    dimensions: (u32, u32),
    file_contents: Vec<u8>,
    limits: Limits,
    color_type: ColorType,
    /// Phantom reference for forward compatibility: future versions may delay
    /// loading the entire file until read_image() and would need to retain
    /// the provided reader type R in the struct
    phantom: PhantomData<R>,
}

/// zune_psd:::PSDDecodeErrors implements Debug but not Display and Error; this wrapper type adds it
#[repr(transparent)]
struct ErrorFromDebug<T>(T);

impl<T> Display for ErrorFromDebug<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}
impl<T> Debug for ErrorFromDebug<T>
where
    T: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}
impl<T> Error for ErrorFromDebug<T> where T: Debug {}

fn map_zune_err(err: zune_psd::errors::PSDDecodeErrors) -> ImageError {
    // PSDDecodeErrors implements Debug instead of not Display
    ImageError::Decoding(DecodingError::new(
        ImageFormatHint::Name("PSD".into()),
        Box::new(ErrorFromDebug(err)),
    ))
}

fn new_zune_decoder<'a>(input: &'a [u8], limits: &Limits) -> zune_psd::PSDDecoder<&'a [u8]> {
    let options = zune_core::options::DecoderOptions::default()
        .set_strict_mode(false)
        .set_max_width(limits.max_image_width.unwrap_or(u32::MAX) as usize)
        .set_max_height(limits.max_image_height.unwrap_or(u32::MAX) as usize);

    zune_psd::PSDDecoder::new_with_options(input, options)
}

/// Read `r` into a vector until EOF or the provided `max_length`
/// bytes have been read. Returns Ok(None) if the input stream
/// length is longer than `max_length`.
fn read_at_most_limit<R: Read>(
    mut r: R,
    max_length: usize,
) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut total_read = 0;

    let mut v: Vec<u8> = vec![0; std::cmp::min(4096, max_length)];

    while total_read < max_length {
        let nread = match r.read(&mut v[total_read..]) {
            Ok(s) => s,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e);
                }
            }
        };
        assert!(nread <= v.len() - total_read, "bad Read implementation");
        total_read += nread;

        if nread == 0 {
            break;
        }

        if total_read > v.len() / 2 {
            v.resize(std::cmp::min(v.len() * 2, max_length), 0);
        }
    }

    if total_read == max_length {
        let mut tmp = [0u8];
        if r.read(&mut tmp)? > 0 {
            // Input stream is longer than `max_length` and cannot be stored entirely
            return Ok(None);
        }
    }
    v.truncate(total_read);
    Ok(Some(v))
}

impl<R> PsdDecoder<R>
where
    R: BufRead,
{
    /// Create a new `PsdDecoder`.
    pub fn with_limits(r: R, limits: Limits) -> Result<PsdDecoder<R>, ImageError> {
        let alloc_limit =
            usize::try_from(limits.max_alloc.unwrap_or(u64::MAX)).unwrap_or(usize::MAX);

        let Some(file_contents) = read_at_most_limit(r, alloc_limit.min(isize::MAX as usize))?
        else {
            return Err(ImageError::Limits(LimitError::from_kind(
                LimitErrorKind::InsufficientMemory,
            )));
        };

        let mut decoder = new_zune_decoder(file_contents.as_slice(), &limits);
        decoder.decode_headers().map_err(map_zune_err)?;

        let bit_depth = decoder.get_bit_depth().expect("headers have been decoded");
        let dimensions_usize = decoder.get_dimensions().expect("headers have been decoded");
        let orig_color_space = decoder.get_colorspace().expect("headers have been decoded");
        let dimensions = match (
            u32::try_from(dimensions_usize.0),
            u32::try_from(dimensions_usize.1),
        ) {
            (Ok(w), Ok(h)) => (w, h),
            _ => {
                return Err(ImageError::Limits(LimitError::from_kind(
                    LimitErrorKind::DimensionError,
                )));
            }
        };

        let color_type = match (orig_color_space, bit_depth) {
            (ZuneColorSpace::Luma, ZuneBitDepth::Eight) => ColorType::L8,
            (ZuneColorSpace::LumaA, ZuneBitDepth::Eight) => ColorType::La8,
            (ZuneColorSpace::RGB, ZuneBitDepth::Eight) => ColorType::Rgb8,
            (ZuneColorSpace::RGBA, ZuneBitDepth::Eight) => ColorType::Rgba8,
            (ZuneColorSpace::Luma, ZuneBitDepth::Sixteen) => ColorType::La16,
            (ZuneColorSpace::LumaA, ZuneBitDepth::Sixteen) => ColorType::La16,
            (ZuneColorSpace::RGB, ZuneBitDepth::Sixteen) => ColorType::Rgb16,
            (ZuneColorSpace::RGBA, ZuneBitDepth::Sixteen) => ColorType::Rgba16,
            _ => {
                return Err(ImageError::Unsupported(
                    UnsupportedError::from_format_and_kind(
                        ImageFormatHint::Name("PSD".into()),
                        UnsupportedErrorKind::GenericFeature(format!(
                            "Unsupported color space {:?} and bit depth {:?}",
                            orig_color_space, bit_depth
                        )),
                    ),
                ));
            }
        };

        Ok(PsdDecoder {
            dimensions,
            color_type,
            limits,
            file_contents,
            phantom: PhantomData,
        })
    }
}

impl<R: BufRead> ImageDecoder for PsdDecoder<R> {
    fn dimensions(&self) -> (u32, u32) {
        self.dimensions
    }

    fn color_type(&self) -> ColorType {
        self.color_type
    }

    fn read_image(self, buf: &mut [u8]) -> ImageResult<()> {
        assert_eq!(u64::try_from(buf.len()), Ok(self.total_bytes()));

        // Limits: an extra copy of size `buf` is temporarily allocated.
        // Warning: zune_psd could have more allocations internally, so this
        // could be an underestimate
        let sat_cast = |x: usize| -> u64 { u64::try_from(x).unwrap_or(u64::MAX) };
        let estimated_alloc =
            sat_cast(buf.len()).saturating_add(sat_cast(self.file_contents.len()));
        if estimated_alloc.saturating_add(1) >= self.limits.max_alloc.unwrap_or(u64::MAX) {
            return Err(ImageError::Limits(LimitError::from_kind(
                LimitErrorKind::InsufficientMemory,
            )));
        }

        let mut decoder = new_zune_decoder(self.file_contents.as_slice(), &self.limits);
        let data = decoder.decode().map_err(map_zune_err)?;
        match data {
            DecodingResult::U8(v) => {
                buf.copy_from_slice(v.as_slice());
            }
            DecodingResult::U16(v) => {
                buf.copy_from_slice(bytemuck::cast_slice::<u16, u8>(v.as_slice()));
            }
            _ => {
                // Bit depth was checked when creating the PsdDecoder
                unreachable!();
            }
        }
        Ok(())
    }

    fn read_image_boxed(self: Box<Self>, buf: &mut [u8]) -> ImageResult<()> {
        (*self).read_image(buf)
    }
}
