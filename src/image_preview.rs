//! Pure image/zip helpers for photo previews: decode+downscale to a JPEG thumbnail, and read one
//! entry's bytes from a zip. No HTTP concern — kept out of `web.rs` so each stays focused.

use std::path::Path;

/// Ceiling on decoded-pixel allocation for one preview: 512 MiB, i.e. ~134 megapixels at RGBA8.
/// Comfortably above any real camera file, low enough that a decompression bomb cannot spike memory.
const PREVIEW_MAX_ALLOC: u64 = 512 * 1024 * 1024;

/// Per-axis ceiling. A single dimension this large is a malformed or hostile header, not a photo.
const PREVIEW_MAX_DIM_PX: u32 = 65_535;

/// Decode limits applied to every preview. Without these, `image` allocates whatever the header
/// claims *before* downscaling, so a small file declaring huge dimensions decides our memory use.
fn preview_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(PREVIEW_MAX_ALLOC);
    limits.max_image_width = Some(PREVIEW_MAX_DIM_PX);
    limits.max_image_height = Some(PREVIEW_MAX_DIM_PX);
    limits
}

/// Decode any supported image, downscale to fit `max_dim` on the longest side, re-encode as JPEG.
/// Decoding is bounded — an oversized or malformed image is an error, never an allocation spike.
pub(crate) fn thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>> {
    thumbnail_jpeg_limited(bytes, max_dim, preview_limits())
}

/// The body of `thumbnail_jpeg`, with the limits injectable so tests can prove they bind.
fn thumbnail_jpeg_limited(
    bytes: &[u8],
    max_dim: u32,
    limits: image::Limits,
) -> anyhow::Result<Vec<u8>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits);
    let img = reader.decode()?;
    let thumb = img.thumbnail(max_dim, max_dim); // preserves aspect ratio, never upsizes past bounds
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg)?;
    Ok(out.into_inner())
}

/// Read one top-level entry's bytes from a zip archive.
pub(crate) fn read_zip_entry(archive_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut entry = zip.by_name(entry_name)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_downscales_and_encodes_jpeg() {
        // a 100x40 image thumbnails to <=32px longest side, and the output decodes as JPEG.
        let img = image::RgbImage::from_pixel(100, 40, image::Rgb([0, 128, 255]));
        let mut src = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut src, image::ImageFormat::Png)
            .unwrap();
        let thumb = thumbnail_jpeg(&src.into_inner(), 32).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert!(decoded.width() <= 32 && decoded.height() <= 32);
        assert!(decoded.width() >= 1);
    }

    /// Encode a solid PNG of the given size.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([1, 2, 3]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn an_allocation_limit_refuses_the_decode_instead_of_growing() {
        let bytes = png(400, 400); // ~480 KB decoded, far above the 1 KiB budget below
        let mut tight = image::Limits::default();
        tight.max_alloc = Some(1024);
        let err = thumbnail_jpeg_limited(&bytes, 32, tight);
        assert!(
            err.is_err(),
            "a decode over the alloc budget must fail, not allocate"
        );
        // The same bytes decode fine under the real preview limits.
        assert!(thumbnail_jpeg(&bytes, 32).is_ok());
    }

    #[test]
    fn a_dimension_limit_refuses_the_decode() {
        let bytes = png(200, 50);
        let mut tight = image::Limits::default();
        tight.max_image_width = Some(100);
        assert!(
            thumbnail_jpeg_limited(&bytes, 32, tight).is_err(),
            "an image wider than the limit must be refused"
        );
    }

    #[test]
    fn the_shipped_limits_admit_a_large_but_real_photo() {
        // 24 MP at RGBA8 is ~96 MB — a normal camera file must not trip the guard.
        let l = preview_limits();
        assert!(l.max_alloc.unwrap() > 96 * 1024 * 1024);
        assert!(l.max_image_width.unwrap() >= 8000);
        assert!(l.max_image_height.unwrap() >= 8000);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(thumbnail_jpeg(b"not an image at all", 32).is_err());
    }
}
