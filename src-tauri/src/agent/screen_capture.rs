use base64::Engine;

#[cfg(windows)]
pub fn capture_primary_jpeg_base64(quality: u8) -> Result<String, String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, RgbaImage};
    use screenshots::Screen;

    let screens = Screen::all().map_err(|e| format!("screenshots list failed: {e}"))?;
    let screen = screens
        .iter()
        .copied()
        .find(|s| s.display_info.is_primary)
        .or_else(|| screens.first().copied())
        .ok_or_else(|| "No screen available for preview capture".to_string())?;

    let rgba = screen
        .capture()
        .map_err(|e| format!("screenshots capture failed: {e}"))?;

    let frame_width = rgba.width() as usize;
    let frame_height = rgba.height() as usize;
    let frame_stride = frame_width * 4;
    let bgra = rgba.into_raw();

    let mut rgba_bytes = vec![0u8; frame_width * frame_height * 4];
    for y in 0..frame_height {
        for x in 0..frame_width {
            let src = y * frame_stride + x * 4;
            let dst = (y * frame_width + x) * 4;
            rgba_bytes[dst] = bgra[src + 2];
            rgba_bytes[dst + 1] = bgra[src + 1];
            rgba_bytes[dst + 2] = bgra[src];
            rgba_bytes[dst + 3] = bgra[src + 3];
        }
    }

    let rgba = RgbaImage::from_raw(frame_width as u32, frame_height as u32, rgba_bytes)
        .ok_or_else(|| "Invalid preview frame buffer".to_string())?;

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
