//! Encoding and Decoding of WBMP Images
//!
//! WBMP (Wireless BitMaP) Format is an image format used by the WAP protocol.
//!
//! # Related Links
//! * <https://en.wikipedia.org/wiki/Wireless_Application_Protocol_Bitmap_Format> - The WBMP format on Wikipedia
//! * <https://www.wapforum.org/what/technical/SPEC-WAESpec-19990524.pdf> - The WAP Specification

use std::io::{BufRead, Seek, Write};

use image::error::{
    DecodingError, EncodingError, ImageFormatHint, UnsupportedError, UnsupportedErrorKind,
};
use image::io::DecodedImageAttributes;
use image::{
    ColorType, ExtendedColorType, ImageDecoder, ImageEncoder, ImageError, ImageLayout, ImageResult,
};

/// Encoder for Wbmp images.
pub struct WbmpEncoder<W> {
    writer: W,
    threshold: u8,
}

impl<W: Write> WbmpEncoder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            threshold: 127_u8,
        }
    }

    pub fn with_threshold(mut self, threshold: u8) -> Self {
        self.threshold = threshold;
        self
    }
}

impl<W: Write> ImageEncoder for WbmpEncoder<W> {
    fn write_image(
        mut self,
        buf: &[u8],
        width: u32,
        height: u32,
        color_type: ExtendedColorType,
    ) -> std::result::Result<(), ImageError> {
        let color = match color_type {
            ExtendedColorType::L8 => wbmp::ColorType::Luma8,
            ExtendedColorType::Rgba8 => wbmp::ColorType::Rgba8,
            _ => {
                return Err(ImageError::Encoding(EncodingError::new(
                    format_hint(),
                    "Unsupported ColorType".to_string(),
                )));
            }
        };

        wbmp::Encoder::new(&mut self.writer)
            .with_threshold(self.threshold)
            .encode(buf, width, height, color)
            .map_err(convert_wbmp_error)
    }
}

/// Decoder for Wbmp images.
pub struct WbmpDecoder<R> {
    inner: wbmp::Decoder<R>,
}

impl<R> WbmpDecoder<R>
where
    R: BufRead + Seek,
{
    /// Create a new `WbmpDecoder`.
    pub fn new(r: R) -> Result<WbmpDecoder<R>, ImageError> {
        let inner = wbmp::Decoder::new(r).map_err(convert_wbmp_error)?;

        Ok(WbmpDecoder { inner })
    }
}

impl<R: BufRead + Seek> ImageDecoder for WbmpDecoder<R> {
    fn peek_layout(&mut self) -> ImageResult<ImageLayout> {
        let dimensions = self.inner.dimensions();
        Ok(ImageLayout {
            color: ColorType::L8,
            width: dimensions.0,
            height: dimensions.1,
        })
    }

    fn original_color_type(&mut self) -> ImageResult<ExtendedColorType> {
        Ok(ExtendedColorType::L1)
    }

    fn read_image(&mut self, buf: &mut [u8]) -> ImageResult<DecodedImageAttributes> {
        let (width, height) = self.inner.dimensions();
        assert_eq!(buf.len(), (width * height) as usize, "Invalid buffer size");

        self.inner
            .read_image_data(buf)
            .map_err(convert_wbmp_error)?;

        Ok(DecodedImageAttributes::default())
    }
}

fn convert_wbmp_error(err: wbmp::error::WbmpError) -> ImageError {
    use wbmp::error::WbmpError;
    match err {
        WbmpError::IoError(inner) => ImageError::IoError(inner),
        WbmpError::UnsupportedType(inner) => {
            ImageError::Unsupported(UnsupportedError::from_format_and_kind(
                format_hint(),
                UnsupportedErrorKind::GenericFeature(format!(
                    "type {inner} is not supported for wbmp images"
                )),
            ))
        }
        WbmpError::UnsupportedHeaders => {
            ImageError::Unsupported(UnsupportedError::from_format_and_kind(
                format_hint(),
                UnsupportedErrorKind::GenericFeature(
                    "Extension headers are not supported for wbmp images".to_string(),
                ),
            ))
        }
        WbmpError::InvalidImageData => ImageError::Encoding(EncodingError::new(format_hint(), err)),
        WbmpError::UsageError(_) => ImageError::Decoding(DecodingError::new(format_hint(), err)),
    }
}

fn format_hint() -> ImageFormatHint {
    ImageFormatHint::Name("WBMP".to_string())
}
