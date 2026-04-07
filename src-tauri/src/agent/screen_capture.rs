use base64::Engine;

#[cfg(windows)]
pub fn capture_primary_jpeg_base64(quality: u8) -> Result<String, String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, RgbaImage};
    use screenshots::Screen;

    let screens = Screen::all().map_err(|e| format!("Screen::all failed: {e}"))?;
    let primary = screens
        .first()
        .ok_or_else(|| "No screen detected".to_string())?;

    let frame = primary
        .capture()
        .map_err(|e| format!("Screen capture failed: {e}"))?;

    let rgba = RgbaImage::from_raw(frame.width(), frame.height(), frame.into_raw())
        .ok_or_else(|| "Invalid frame buffer".to_string())?;

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
    encoder
        .encode_image(&DynamicImage::ImageRgba8(rgba))
        .map_err(|e| format!("JPEG encode failed: {e}"))?;

    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

#[cfg(not(windows))]
pub fn capture_primary_jpeg_base64(_quality: u8) -> Result<String, String> {
    Err("Screen capture currently supported on Windows only".to_string())
}
