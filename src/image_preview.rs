//! Pure image/zip helpers for photo previews: decode+downscale to a JPEG thumbnail, and read one
//! entry's bytes from a zip. No HTTP concern — kept out of `web.rs` so each stays focused.

use std::path::Path;

/// Decode any supported image, downscale to fit `max_dim` on the longest side, re-encode as JPEG.
pub(crate) fn thumbnail_jpeg(bytes: &[u8], max_dim: u32) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
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
        image::DynamicImage::ImageRgb8(img).write_to(&mut src, image::ImageFormat::Png).unwrap();
        let thumb = thumbnail_jpeg(&src.into_inner(), 32).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert!(decoded.width() <= 32 && decoded.height() <= 32);
        assert!(decoded.width() >= 1);
    }
}
