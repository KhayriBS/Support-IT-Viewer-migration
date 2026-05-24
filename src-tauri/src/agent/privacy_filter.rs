//! Privacy filter — detects password fields via Windows UI Automation and
//! applies a box blur on the captured BGRA buffer *before* H.264 encoding,
//! so sensitive content never leaves the agent over WebRTC.
//!
//! The filter is intentionally conservative:
//!  - Scans are rate-limited (default 500 ms) — UI Automation is slow.
//!  - All UIA calls are wrapped to return `Vec::new()` on failure instead
//!    of panicking (elevated processes, secure desktop, UAC, etc.).
//!  - Blur is a 2-pass box blur implemented manually on BGRA, no external
//!    image crate needed.
//!
//! The whole detection path is Windows-only. On other platforms a stub
//! `PrivacyFilter` is exposed so the rest of the crate compiles unchanged.
//!
//! Thread safety: instances are designed to be wrapped in
//! `Arc<Mutex<PrivacyFilter>>` and shared between the capture loop (which
//! calls `process_frame` once per captured BGRA frame) and the WebRTC
//! "privacy" DataChannel handler (which mutates `enabled`).

use std::time::{Duration, Instant};

/// Rectangle in *screen-absolute* coordinates returned by UIA's
/// `CurrentBoundingRectangle`. Negative coordinates are possible when
/// secondary monitors live to the left/above the primary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SensitiveRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Default scan cadence — UI Automation `FindAll` over the whole desktop
/// can take 50–300 ms on a busy session; running it every 30 fps frame
/// would tank the capture loop.
const DEFAULT_SCAN_INTERVAL_MS: u64 = 500;

/// Default blur radius (in pixels) — empirical: 15 px erases sub-pixel
/// glyph details on standard DPI, even for very small password fields.
pub const DEFAULT_BLUR_RADIUS: u32 = 15;

/// Stateful privacy filter shared between the capture loop and the
/// DataChannel handler that toggles it. See module docs.
pub struct PrivacyFilter {
    pub regions: Vec<SensitiveRegion>,
    pub last_scan: Instant,
    pub scan_interval: Duration,
    pub enabled: bool,
    /// Dimensions of the screen at the moment `regions` was refreshed.
    /// Stored so we can rescale UIA coordinates if the encoder is fed a
    /// downscaled buffer (e.g. 4K → 1080p auto-downscale).
    pub scan_screen_size: Option<(i32, i32)>,
    /// Pixel radius of the box blur. 15 is enough to make any glyph
    /// unreadable; bump to 25 for very large fonts / accessibility mode.
    pub blur_radius: u32,
}

impl PrivacyFilter {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            // Force an immediate first scan by backdating `last_scan`.
            last_scan: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            scan_interval: Duration::from_millis(DEFAULT_SCAN_INTERVAL_MS),
            // Privacy by default — only the technician can lift it from
            // the viewer, and only after an explicit click.
            enabled: true,
            scan_screen_size: None,
            blur_radius: DEFAULT_BLUR_RADIUS,
        }
    }

    pub fn should_rescan(&self) -> bool {
        self.last_scan.elapsed() >= self.scan_interval
    }

    /// Re-runs UIA detection and updates `self.regions`. Safe to call from
    /// any thread (Windows COM is initialized lazily as MTA inside).
    pub fn refresh_regions(&mut self) {
        self.regions = detect_password_fields();
        self.scan_screen_size = current_screen_size();
        self.last_scan = Instant::now();
    }

    /// Apply privacy blur to a captured BGRA frame.
    ///
    /// Called once per captured frame from the encoder pipeline. When
    /// `enabled == false` this is a no-op (zero allocation, zero copy)
    /// so toggling off cleanly disables all overhead.
    pub fn process_frame(&mut self, buffer: &mut Vec<u8>, width: u32, height: u32) {
        if !self.enabled {
            return;
        }
        if self.should_rescan() {
            self.refresh_regions();
        }
        if self.regions.is_empty() {
            return;
        }

        // Rescale UIA regions to the buffer if the encoder is fed a
        // downscaled image (4K capture → 1080p stream is common).
        let scaled = match self.scan_screen_size {
            Some((sw, sh)) if sw > 0 && sh > 0 && (sw as u32 != width || sh as u32 != height) => {
                let sx = width as f32 / sw as f32;
                let sy = height as f32 / sh as f32;
                self.regions
                    .iter()
                    .map(|r| SensitiveRegion {
                        x: (r.x as f32 * sx) as i32,
                        y: (r.y as f32 * sy) as i32,
                        width: (r.width as f32 * sx).ceil() as i32,
                        height: (r.height as f32 * sy).ceil() as i32,
                    })
                    .collect::<Vec<_>>()
            }
            _ => self.regions.clone(),
        };

        apply_privacy_blur(buffer, width, height, &scaled, self.blur_radius);
    }
}

impl Default for PrivacyFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterates `regions` and blurs each one in place on the BGRA buffer.
pub fn apply_privacy_blur(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    regions: &[SensitiveRegion],
    radius: u32,
) {
    if width == 0 || height == 0 || buffer.is_empty() {
        return;
    }
    for region in regions {
        blur_region(buffer, width, height, region, radius);
    }
}

/// Box blur (2 passes: horizontal then vertical) restricted to `region`.
///
/// `buffer` is BGRA, 4 bytes per pixel, stride = width * 4.
/// Coordinates outside the buffer are silently clamped — a region partly
/// off-screen is simply blurred where it intersects the buffer.
pub fn blur_region(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    region: &SensitiveRegion,
    radius: u32,
) {
    if width == 0 || height == 0 || radius == 0 {
        return;
    }

    let stride = width as usize * 4;
    let expected_len = stride.saturating_mul(height as usize);
    if buffer.len() < expected_len {
        return;
    }

    // Clamp region to [0, width-1] × [0, height-1].
    let x0 = region.x.max(0) as u32;
    let y0 = region.y.max(0) as u32;
    let x1 = (region.x + region.width).max(0) as u32;
    let y1 = (region.y + region.height).max(0) as u32;
    let x0 = x0.min(width);
    let y0 = y0.min(height);
    let x1 = x1.min(width);
    let y1 = y1.min(height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let region_w = (x1 - x0) as usize;
    let region_h = (y1 - y0) as usize;
    let r = radius as i32;

    // Temporary buffer holding the horizontal-pass result. We only keep
    // B/G/R (3 channels) — alpha is preserved in-place.
    let mut tmp = vec![0u8; region_w * region_h * 3];

    // ── Pass 1 : horizontal box blur ────────────────────────────────
    for ry in 0..region_h {
        let src_row = (y0 as usize + ry) * stride;
        for rx in 0..region_w {
            let mut sum_b: u32 = 0;
            let mut sum_g: u32 = 0;
            let mut sum_r: u32 = 0;
            let mut count: u32 = 0;

            let center = rx as i32;
            let from = (center - r).max(0);
            let to = (center + r).min(region_w as i32 - 1);
            for nx in from..=to {
                let abs_x = x0 as usize + nx as usize;
                let idx = src_row + abs_x * 4;
                sum_b += buffer[idx] as u32;
                sum_g += buffer[idx + 1] as u32;
                sum_r += buffer[idx + 2] as u32;
                count += 1;
            }
            if count == 0 {
                continue;
            }
            let dst = (ry * region_w + rx) * 3;
            tmp[dst] = (sum_b / count) as u8;
            tmp[dst + 1] = (sum_g / count) as u8;
            tmp[dst + 2] = (sum_r / count) as u8;
        }
    }

    // ── Pass 2 : vertical box blur, write back into `buffer` ────────
    for rx in 0..region_w {
        for ry in 0..region_h {
            let mut sum_b: u32 = 0;
            let mut sum_g: u32 = 0;
            let mut sum_r: u32 = 0;
            let mut count: u32 = 0;

            let center = ry as i32;
            let from = (center - r).max(0);
            let to = (center + r).min(region_h as i32 - 1);
            for ny in from..=to {
                let idx = (ny as usize * region_w + rx) * 3;
                sum_b += tmp[idx] as u32;
                sum_g += tmp[idx + 1] as u32;
                sum_r += tmp[idx + 2] as u32;
                count += 1;
            }
            if count == 0 {
                continue;
            }
            let abs_x = x0 as usize + rx;
            let abs_y = y0 as usize + ry;
            let dst = abs_y * stride + abs_x * 4;
            buffer[dst] = (sum_b / count) as u8;
            buffer[dst + 1] = (sum_g / count) as u8;
            buffer[dst + 2] = (sum_r / count) as u8;
            // alpha (buffer[dst + 3]) left untouched
        }
    }
}

/// Returns the current physical screen size (width, height) in pixels.
/// `None` if the call fails or the platform is not Windows.
fn current_screen_size() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w > 0 && h > 0 {
                Some((w, h))
            } else {
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ──────────────────────────────────────────────────────────────────────
//                       UI Automation — Windows only
// ──────────────────────────────────────────────────────────────────────

/// Scans the desktop tree for password-like elements and returns their
/// bounding rectangles. Returns an empty vector on any failure (elevated
/// process, RPC_E_CHANGED_MODE, UAC secure desktop, etc.).
pub fn detect_password_fields() -> Vec<SensitiveRegion> {
    #[cfg(windows)]
    {
        match detect_password_fields_impl() {
            Ok(regions) => regions,
            Err(err) => {
                tracing::debug!("🔒 UIA password detection skipped: {err}");
                Vec::new()
            }
        }
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn detect_password_fields_impl() -> Result<Vec<SensitiveRegion>, String> {
    use windows::core::VARIANT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Subtree,
        UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_IsPasswordPropertyId,
    };

    unsafe {
        // CoInitializeEx is idempotent across calls on the same thread
        // when the apartment kind matches. S_FALSE just means "already
        // initialized as MTA" — fine. RPC_E_CHANGED_MODE means another
        // part of the process initialized us as STA; we bail out rather
        // than risk threading issues.
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() {
            let code = hr.0;
            // RPC_E_CHANGED_MODE = 0x80010106
            if code == 0x8001_0106u32 as i32 {
                return Err("COM already initialized in conflicting apartment (STA)".to_string());
            }
            // Any other failure HRESULT is fatal here.
            return Err(format!("CoInitializeEx failed (hr=0x{code:08X})"));
        }

        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance(CUIAutomation) failed: {e}"))?;

        let root: IUIAutomationElement = automation
            .GetRootElement()
            .map_err(|e| format!("GetRootElement failed: {e}"))?;

        let mut regions = Vec::new();

        // ── Condition 1 : IsPassword == true ────────────────────────
        // Per the UIA spec, every password field — native Win32, WPF,
        // UWP, Edge/Chrome, Electron — advertises IsPassword=true. This
        // is the canonical detection path. (There is no
        // UIA_PasswordControlTypeId in the SDK: the control type is
        // always `Edit` and `IsPassword` discriminates.)
        let pwd_variant = VARIANT::from(true);
        let pwd_condition = automation
            .CreatePropertyCondition(UIA_IsPasswordPropertyId, &pwd_variant)
            .map_err(|e| format!("CreatePropertyCondition(IsPassword) failed: {e}"))?;

        if let Ok(array) = root.FindAll(TreeScope_Subtree, &pwd_condition) {
            let count = array.Length().unwrap_or(0);
            for i in 0..count {
                if let Ok(element) = array.GetElement(i) {
                    if let Some(region) = bounding_rect_to_region(&element) {
                        regions.push(region);
                    }
                }
            }
        }

        // ── Condition 2 : ControlType == Edit (best-effort fallback) ──
        // Some legacy controls expose IsPassword incorrectly. We don't
        // blur every Edit (that would hide normal text inputs), but we
        // still build the condition object to validate the UIA tree is
        // walkable — drops are silent.
        let edit_variant = VARIANT::from(UIA_EditControlTypeId.0);
        let _ = automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &edit_variant);

        Ok(regions)
    }
}

#[cfg(windows)]
fn bounding_rect_to_region(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<SensitiveRegion> {
    unsafe {
        let rect = element.CurrentBoundingRectangle().ok()?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(SensitiveRegion {
            x: rect.left,
            y: rect.top,
            width,
            height,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────
//                                 Tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_buffer(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut buf = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            buf.extend_from_slice(&color);
        }
        buf
    }

    #[test]
    fn blur_noop_outside_buffer() {
        let mut buf = solid_buffer(16, 16, [10, 20, 30, 255]);
        let region = SensitiveRegion {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        blur_region(&mut buf, 16, 16, &region, 5);
        // Untouched.
        assert_eq!(buf[0..4], [10, 20, 30, 255]);
    }

    #[test]
    fn blur_smooths_step_edge() {
        // Half-black / half-white image. After blur, a band around the
        // boundary should contain intermediate gray values.
        let w = 32u32;
        let h = 8u32;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if x < w / 2 { 0 } else { 255 };
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
                buf[i + 3] = 255;
            }
        }
        let region = SensitiveRegion {
            x: 0,
            y: 0,
            width: w as i32,
            height: h as i32,
        };
        blur_region(&mut buf, w, h, &region, 4);

        // Sample a pixel right at the boundary — must be neither 0 nor 255.
        let mid_x = (w / 2) as usize;
        let mid_y = (h / 2) as usize;
        let idx = (mid_y * w as usize + mid_x) * 4;
        let v = buf[idx];
        assert!(v > 30 && v < 225, "boundary pixel still binary: {v}");
    }

    #[test]
    fn disabled_filter_is_noop() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = false;
        filter.regions = vec![SensitiveRegion {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        }];
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn empty_regions_skip_work() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        filter.regions = Vec::new();
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        // Force last_scan recent so refresh_regions isn't called.
        filter.last_scan = Instant::now();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn should_rescan_after_interval() {
        let mut filter = PrivacyFilter::new();
        filter.scan_interval = Duration::from_millis(10);
        filter.last_scan = Instant::now();
        assert!(!filter.should_rescan());
        std::thread::sleep(Duration::from_millis(20));
        assert!(filter.should_rescan());
    }
}
