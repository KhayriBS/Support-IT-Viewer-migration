use base64::Engine;

#[cfg(windows)]
pub fn capture_primary_jpeg_base64(quality: u8) -> Result<String, String> {
    use super::desktop_duplication::DxgiDesktopDuplicator;
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, RgbaImage};
    let mut capturer = DxgiDesktopDuplicator::new()?;
    let frame = capturer
        .capture_next_frame(250)?
        .ok_or_else(|| "DXGI capture timeout".to_string())?;

    let mut rgba_bytes = vec![0u8; frame.width * frame.height * 4];
    for y in 0..frame.height {
        for x in 0..frame.width {
            let src = y * frame.stride + x * 4;
            let dst = (y * frame.width + x) * 4;
            rgba_bytes[dst] = frame.bgra[src + 2];
            rgba_bytes[dst + 1] = frame.bgra[src + 1];
            rgba_bytes[dst + 2] = frame.bgra[src];
            rgba_bytes[dst + 3] = frame.bgra[src + 3];
        }
    }

    let rgba = RgbaImage::from_raw(frame.width as u32, frame.height as u32, rgba_bytes)
        .ok_or_else(|| "Invalid DXGI frame buffer".to_string())?;

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
