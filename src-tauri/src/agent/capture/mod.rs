use std::env;

use super::desktop_duplication::DesktopFrame;

mod dxgi;
mod wgc;

pub trait ScreenCapturer {
    fn capture_next_frame(&mut self, timeout_ms: u32) -> Result<Option<DesktopFrame>, String>;
}

pub fn create_default_capturer() -> Result<Box<dyn ScreenCapturer + Send>, String> {
    let backend = env::var("LUMIERE_CAPTURE_BACKEND")
        .ok()
        .unwrap_or_else(|| "auto".to_string())
        .trim()
        .to_ascii_lowercase();

    match backend.as_str() {
        "dxgi" => {
            println!("Capture backend selected: DXGI");
            Ok(Box::new(dxgi::DxgiCapturer::new()?))
        }
        "wgc" => {
            println!("Capture backend selected: WGC");
            Ok(Box::new(wgc::WgcCapturer::new()?))
        }
        "auto" | "" => {
            // Prefer DXGI when available for lower copy overhead, fall back to WGC path.
            match dxgi::DxgiCapturer::new() {
                Ok(capturer) => {
                    println!("Capture backend auto-selected: DXGI");
                    Ok(Box::new(capturer))
                }
                Err(dxgi_err) => {
                    eprintln!("DXGI unavailable in auto mode: {dxgi_err}; trying WGC backend");
                    match wgc::WgcCapturer::new() {
                        Ok(capturer) => {
                            println!("Capture backend auto-selected: WGC");
                            Ok(Box::new(capturer))
                        }
                        Err(wgc_err) => Err(format!(
                            "No capture backend available (DXGI error: {dxgi_err}; WGC error: {wgc_err})"
                        )),
                    }
                }
            }
        }
        other => {
            eprintln!("Unknown capture backend '{other}', falling back to auto");
            match dxgi::DxgiCapturer::new() {
                Ok(capturer) => {
                    println!("Capture backend auto-selected: DXGI");
                    Ok(Box::new(capturer))
                }
                Err(dxgi_err) => {
                    eprintln!("DXGI unavailable in auto fallback: {dxgi_err}; trying WGC backend");
                    match wgc::WgcCapturer::new() {
                        Ok(capturer) => {
                            println!("Capture backend auto-selected: WGC");
                            Ok(Box::new(capturer))
                        }
                        Err(wgc_err) => Err(format!(
                            "No capture backend available (DXGI error: {dxgi_err}; WGC error: {wgc_err})"
                        )),
                    }
                }
            }
        }
    }
}

pub fn capture_primary_screen_even_bgra(
    capturer: &mut dyn ScreenCapturer,
    scale_target: Option<(usize, usize)>,
) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    let Some(frame) = capturer.capture_next_frame(16)? else {
        return Ok(None);
    };

    let prepared = normalize_frame_for_stream(frame, scale_target)?;
    Ok(Some(prepared))
}

fn normalize_frame_for_stream(
    frame: DesktopFrame,
    scale_target: Option<(usize, usize)>,
) -> Result<(usize, usize, Vec<u8>), String> {
    let frame = if let Some((requested_width, requested_height)) = scale_target {
        let (target_width, target_height) = resolve_scaled_dimensions(
            frame.width,
            frame.height,
            requested_width,
            requested_height,
        );
        frame.resize_bgra_nearest(target_width, target_height)
    } else if should_auto_downscale() && (frame.width > 1920 || frame.height > 1080) {
        // Auto-downscale high-resolution captures (4K, ultrawide, etc.) to 1080p.
        let aspect = frame.width as f64 / frame.height.max(1) as f64;
        let (tw, th) = if aspect >= 1.0 {
            let w = 1920usize;
            let h = ((w as f64 / aspect).round() as usize).max(2);
            (w, h)
        } else {
            let h = 1080usize;
            let w = ((h as f64 * aspect).round() as usize).max(2);
            (w, h)
        };
        frame.resize_bgra_nearest(tw & !1, th & !1)
    } else {
        frame
    };

    let (width, height, bgra) = frame.into_even_bgra();
    if width < 2 || height < 2 {
        return Err("Captured frame is too small".to_string());
    }

    Ok((width, height, bgra))
}

pub fn resolve_scale_request() -> Option<(usize, usize)> {
    let requested_width = env::var("LUMIERE_STREAM_WIDTH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 320);
    let requested_height = env::var("LUMIERE_STREAM_HEIGHT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 180);

    match (requested_width, requested_height) {
        (Some(width), Some(height)) => Some((width & !1, height & !1)),
        (Some(width), None) => Some((width & !1, 0)),
        (None, Some(height)) => Some((0, height & !1)),
        (None, None) => None,
    }
}

fn should_auto_downscale() -> bool {
    !matches!(
        env::var("LUMIERE_STREAM_NOSCALE")
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn resolve_scaled_dimensions(
    source_width: usize,
    source_height: usize,
    requested_width: usize,
    requested_height: usize,
) -> (usize, usize) {
    let aspect = source_width as f64 / source_height.max(1) as f64;

    let (target_width, target_height) = match (requested_width, requested_height) {
        (width, height) if width > 0 && height > 0 => (width, height),
        (width, 0) if width > 0 => {
            let height = ((width as f64 / aspect).round() as usize).max(2);
            (width, height)
        }
        (0, height) if height > 0 => {
            let width = ((height as f64 * aspect).round() as usize).max(2);
            (width, height)
        }
        _ => (source_width, source_height),
    };

    ((target_width.max(2)) & !1, (target_height.max(2)) & !1)
}
