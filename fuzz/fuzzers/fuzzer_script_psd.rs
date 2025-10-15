#![no_main]
#[macro_use]
extern crate libfuzzer_sys;

use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let reader = Cursor::new(data);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(1024 * 1024); // 1 MiB
    let Ok(decoder) = image_extras::psd::PsdDecoder::with_limits(reader, limits) else {
        return;
    };
    let _ = std::hint::black_box(image::DynamicImage::from_decoder(decoder));
});
