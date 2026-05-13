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

/// Capture l'ecran primaire et l'encode en JPEG base64 APRES downscale a une
/// largeur maximale donnee. Utilise par l'IA pour rester sous la limite SCTP
/// du DataChannel (~256 KB cote Chrome).
///
/// Exemples de tailles pour un ecran 1920x1080 :
///   • max_width=1280, q=50 : ~ 60-100 KB base64   ← bonne valeur pour IA
///   • max_width=960,  q=40 : ~ 35-60  KB base64   ← tres safe
///   • max_width=1920, q=70 : ~ 250-400 KB base64  ← peut casser le DataChannel
#[cfg(windows)]
pub fn capture_primary_jpeg_base64_scaled(
    max_width: u32,
    quality: u8,
) -> Result<(String, u32, u32), String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    use image::{DynamicImage, RgbaImage};
    use screenshots::Screen;

    let screens = Screen::all().map_err(|e| format!("screenshots list failed: {e}"))?;
    let screen = screens
        .iter()
        .copied()
        .find(|s| s.display_info.is_primary)
        .or_else(|| screens.first().copied())
        .ok_or_else(|| "No screen available for preview capture".to_string())?;

    let rgba_native = screen
        .capture()
        .map_err(|e| format!("screenshots capture failed: {e}"))?;

    let native_w = rgba_native.width();
    let native_h = rgba_native.height();
    let stride = native_w as usize * 4;
    let bgra = rgba_native.into_raw();

    // BGRA -> RGBA
    let mut rgba_bytes = vec![0u8; (native_w * native_h * 4) as usize];
    for y in 0..native_h as usize {
        for x in 0..native_w as usize {
            let src = y * stride + x * 4;
            let dst = (y * native_w as usize + x) * 4;
            rgba_bytes[dst] = bgra[src + 2];
            rgba_bytes[dst + 1] = bgra[src + 1];
            rgba_bytes[dst + 2] = bgra[src];
            rgba_bytes[dst + 3] = bgra[src + 3];
        }
    }

    let mut img = DynamicImage::ImageRgba8(
        RgbaImage::from_raw(native_w, native_h, rgba_bytes)
            .ok_or_else(|| "Invalid screen buffer".to_string())?,
    );

    // Downscale si l'ecran est plus large que max_width (ratio preserve).
    if img.width() > max_width {
        let ratio = max_width as f32 / img.width() as f32;
        let new_h = ((img.height() as f32) * ratio).round() as u32;
        img = img.resize(max_width, new_h, FilterType::Triangle);
    }

    let final_w = img.width();
    let final_h = img.height();

    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;

    Ok((
        base64::engine::general_purpose::STANDARD.encode(out),
        final_w,
        final_h,
    ))
}

#[cfg(not(windows))]
pub fn capture_primary_jpeg_base64_scaled(
    _max_width: u32,
    _quality: u8,
) -> Result<(String, u32, u32), String> {
    Err("Screen capture currently supported on Windows only".to_string())
}
