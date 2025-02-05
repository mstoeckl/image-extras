//! This crate provides additional formats for the image crate.
//!
//! The enabled formats are controlled via Cargo features:
//! ```toml
//! [dependencies]
//! image_extras = { version = "0.1", features = ["pcx"] }
//! ```
//!
//! And you must also call the `register` function at program startup:
//!
//!  ```rust,no_run
//! image_extras::register();
//!
//! // Now you can use the image crate as normal
//! let img = image::open("path/to/image.pcx").unwrap();
//! ```

#[cfg(feature = "pcx")]
pub mod pcx;

#[cfg(feature = "xbm")]
pub mod xbm;

/// Register all enabled extra formats with the image crate.
pub fn register() {
    image::hooks::register_decoding_hook(
        image::ImageFormat::Pcx,
        Box::new(|r| Ok(Box::new(pcx::PCXDecoder::new(r)?))),
    );
    #[cfg(feature = "xbm")]
    image::hooks::register_decoding_hook(
        image::ImageFormat::Xbm,
        Box::new(|r| Ok(Box::new(xbm::XbmDecoder::new(r)?))),
    );
}
