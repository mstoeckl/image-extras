//! Decoding of OpenRaster Images (*.ora)
//!
//! OpenRaster is an a file format used to communicate layered images; the
//! decoder provided herein only extracts and displays the final merged raster
//! image cached by the OpenRaster file, and does not expose the details of
//! layers (which may be either raster or vector graphics) or render the merged
//! image itself.
//!
//! # Related Links
//! * <https://en.wikipedia.org/wiki/OpenRaster> - The OpenRaster format on Wikipedia
//! * <https://www.openraster.org/> - OpenRaster specification

use image::codecs::png::PngDecoder;
use image::error::{
    DecodingError, ImageFormatHint, ParameterError, ParameterErrorKind, UnsupportedError,
};
use image::io::{DecodedImageAttributes, DecoderAttributes};
use image::metadata::Orientation;
use image::{
    ColorType, ExtendedColorType, ImageDecoder, ImageError, ImageLayout, ImageResult, Limits,
};
use ouroboros::self_referencing;
use std::io::{self, BufReader, Cursor, Read, Seek};
use std::marker::PhantomData;
use zip::read::{ZipArchive, ZipFile};

enum OraState<'a, R>
where
    R: Read + Seek + 'a,
{
    Init(R),
    Main(PngDecoder<BufReader<SeekableArchiveFile<'a, R>>>),
    /// Null state, used if the transition from Init to Main fails
    Failed,
}

pub struct OpenRasterDecoder<'a, R>
where
    R: Read + Seek + 'a,
{
    state: OraState<'a, R>,
    limits: Option<Limits>,
}

fn openraster_format_hint() -> ImageFormatHint {
    ImageFormatHint::Name("OpenRaster".into())
}

/// Adjust the format of the PngDecoder's errors to indicate OpenRaster instead
fn set_ora_image_type(err: ImageError) -> ImageError {
    match err {
        ImageError::Decoding(e) => {
            // DecodingError does not directly expose the underlying type,
            // so nest the error
            ImageError::Decoding(DecodingError::new(openraster_format_hint(), e))
        }
        ImageError::Encoding(_) => {
            // Should not be encoding any files
            unreachable!();
        }
        ImageError::Parameter(e) => ImageError::Parameter(e),
        ImageError::Limits(e) => ImageError::Limits(e),
        ImageError::Unsupported(e) => ImageError::Unsupported(
            UnsupportedError::from_format_and_kind(openraster_format_hint(), e.kind()),
        ),
        ImageError::IoError(e) => ImageError::IoError(e),
    }
}

#[self_referencing]
struct SeekableArchiveCore<'a, R: Read + Seek + 'a> {
    archive: ZipArchive<R>,
    #[covariant]
    #[borrows(mut archive)]
    file: ZipFile<'this, R>,
    lifetime_helper: PhantomData<&'a R>,
}

/// The zip crate does not provide a seekable reader that works on compressed
/// entries, while png::Decoder requires the Seek bound (but does not currently
/// use it). This structure implements Seek by reopening and reading the zip
/// archive entry whenever it seeks backwards.
struct SeekableArchiveFile<'a, R: Read + Seek + 'a> {
    core: Option<SeekableArchiveCore<'a, R>>,
    file_index: usize,
    position: u64,
    file_size: u64,
}

impl<'a, R: Read + Seek + 'a> SeekableArchiveFile<'a, R> {
    fn new(
        archive: ZipArchive<R>,
        file_index: usize,
    ) -> Result<SeekableArchiveFile<'a, R>, io::Error> {
        let core = SeekableArchiveCore::try_new(archive, |x| x.by_index(file_index), PhantomData)
            .map_err(|x| io::Error::other(format!("failed to open: {:?}", x)))?;
        let file_size = core.with_file(|file| file.size());
        Ok(SeekableArchiveFile {
            core: Some(core),
            file_index,
            position: 0,
            file_size,
        })
    }
}

impl<R: Read + Seek> Read for SeekableArchiveFile<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let res = self
            .core
            .as_mut()
            .unwrap()
            .with_file_mut(|file| file.read(buf));
        let nread = res?;
        self.position
            .checked_add(nread as u64)
            .ok_or_else(|| io::Error::other("seek position overflow"))?;
        Ok(nread)
    }
}

impl<R: Read + Seek> Seek for SeekableArchiveFile<'_, R> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let target_pos = match pos {
            io::SeekFrom::Start(offset) => offset,
            io::SeekFrom::End(offset) => self
                .file_size
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::other("seek position over or underflow"))?,
            io::SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or_else(|| io::Error::other("seek position over or underflow"))?,
        };

        if target_pos < self.position {
            let core = self.core.take();
            let archive = core.unwrap().into_heads().archive;

            self.core = Some(
                SeekableArchiveCore::try_new(archive, |x| x.by_index(self.file_index), PhantomData)
                    .map_err(|x| io::Error::other(format!("failed to reopen: {:?}", x)))?,
            );
        }
        while self.position < target_pos {
            const TMP_LEN: usize = 1024;
            let mut tmp = [0_u8; TMP_LEN];
            let cur_pos = self.position;
            let nr = self
                .read(&mut tmp[..std::cmp::min(TMP_LEN as u64, target_pos - cur_pos) as usize])?;
            if nr == 0 {
                return Err(io::Error::other("unexpected eof when seeking"));
            }
            self.position += nr as u64;
        }

        Ok(0)
    }
}

impl<'a, R> OpenRasterDecoder<'a, R>
where
    R: Read + Seek + 'a,
{
    /// Create a new `OpenRasterDecoder`
    ///
    /// Warning: While decoding limits apply to the header parsing and decoding
    /// of the merged imaged component (a PNG file inside the ZIP archive that
    /// forms an OpenRaster file), memory constraints on the ZIP file decoding
    /// process have not yet been implemented; input ZIP files with very many
    /// entries may require significant amounts of memory to read.
    pub fn new(r: R) -> OpenRasterDecoder<'a, R> {
        OpenRasterDecoder {
            state: OraState::Init(r),
            limits: None,
        }
    }
}

impl<'a, R: Read + Seek + 'a> ImageDecoder for OpenRasterDecoder<'a, R> {
    fn peek_layout(&mut self) -> ImageResult<ImageLayout> {
        if matches!(self.state, OraState::Init(_)) {
            let OraState::Init(r) = std::mem::replace(&mut self.state, OraState::Failed) else {
                unreachable!();
            };

            let mut archive = ZipArchive::new(r).map_err(|e| {
                ImageError::Decoding(DecodingError::new(openraster_format_hint(), e))
            })?;

            /* Verify that this _is_ an OpenRaster file, and not some unrelated ZIP archive */
            let mimetype_index = archive.index_for_name("mimetype").ok_or_else(|| {
                ImageError::Decoding(DecodingError::new(
                    openraster_format_hint(),
                    "OpenRaster images should contain a mimetype subfile",
                ))
            })?;

            let mut mimetype_file = archive.by_index(mimetype_index).map_err(|x| {
                ImageError::Decoding(DecodingError::new(openraster_format_hint(), x))
            })?;

            const EXPECTED_MIMETYPE: &str = "image/openraster";
            let mut tmp = [0u8; EXPECTED_MIMETYPE.len()];

            mimetype_file.read_exact(&mut tmp)?;

            if tmp != EXPECTED_MIMETYPE.as_bytes()
                || mimetype_file.size() != EXPECTED_MIMETYPE.len() as u64
            {
                return Err(ImageError::Decoding(DecodingError::new(
                    openraster_format_hint(),
                    "Image did not have correct mimetype subentry to be identified as OpenRaster",
                )));
            }

            drop(mimetype_file);

            let mergedimage_index = archive.index_for_name("mergedimage.png").ok_or_else(|| {
                ImageError::Decoding(DecodingError::new(
                    openraster_format_hint(),
                    "OpenRaster image missing mergedimage.png entry",
                ))
            })?;

            let file = SeekableArchiveFile::new(archive, mergedimage_index)?;
            let decoder = if let Some(limits) = self.limits.take() {
                PngDecoder::with_limits(BufReader::new(file), limits)
            } else {
                PngDecoder::new(BufReader::new(file))
            };

            self.state = OraState::Main(decoder);
        }

        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::NoMoreData,
            )));
        };
        decoder.peek_layout().map_err(set_ora_image_type)
    }

    fn original_color_type(&mut self) -> ImageResult<ExtendedColorType> {
        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::Generic("Need to call peek_layout() first".into()),
            )));
        };
        decoder.original_color_type().map_err(set_ora_image_type)
    }

    fn attributes(&self) -> DecoderAttributes {
        // TODO: This is a hack; consider hard coding attributes
        let empty = Cursor::new(&[]);
        let mut attrib = PngDecoder::new(empty).attributes();
        // The previous image should have only one part
        attrib.is_animated = false;
        attrib.is_sequence = false;
        attrib
    }

    fn set_limits(&mut self, limits: Limits) -> ImageResult<()> {
        // Warning: this does not account for any ZIP reading overhead
        if let OraState::Main(decoder) = &mut self.state {
            decoder.set_limits(limits)?;
        } else {
            self.limits = Some(limits);
        }
        Ok(())
    }

    fn icc_profile(&mut self) -> ImageResult<Option<Vec<u8>>> {
        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::Generic("Need to call peek_layout() first".into()),
            )));
        };
        decoder.icc_profile().map_err(set_ora_image_type)
    }

    fn exif_metadata(&mut self) -> ImageResult<Option<Vec<u8>>> {
        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::Generic("Need to call peek_layout() first".into()),
            )));
        };
        decoder.exif_metadata().map_err(set_ora_image_type)
    }

    fn xmp_metadata(&mut self) -> ImageResult<Option<Vec<u8>>> {
        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::Generic("Need to call peek_layout() first".into()),
            )));
        };
        decoder.xmp_metadata().map_err(set_ora_image_type)
    }

    fn iptc_metadata(&mut self) -> ImageResult<Option<Vec<u8>>> {
        let OraState::Main(decoder) = &mut self.state else {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::Generic("Need to call peek_layout() first".into()),
            )));
        };
        decoder.iptc_metadata().map_err(set_ora_image_type)
    }

    fn read_image(&mut self, buf: &mut [u8]) -> ImageResult<DecodedImageAttributes> {
        self.peek_layout()?;
        let OraState::Main(decoder) = &mut self.state else {
            unreachable!();
        };
        decoder.read_image(buf).map_err(set_ora_image_type)
        // TODO: mark self.state so that, if the PNG images has APNG frames, the decoder rejects them
    }
}
