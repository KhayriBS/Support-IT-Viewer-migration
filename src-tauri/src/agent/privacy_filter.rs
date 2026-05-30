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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
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

/// Stable identifier for a UIA element across scans — the array of
/// `i32` returned by `IUIAutomationElement::GetRuntimeId`. Two scans
/// returning the same `RuntimeId` are guaranteed to refer to the same
/// element (per Microsoft's UIA contract).
type RuntimeId = Vec<i32>;

/// Internal record tracking a previously-detected password input.
///
/// We key by `RuntimeId` (stable UIA identity) rather than by screen
/// position because:
///  - A password field can *move* between scans (scroll, reflow, virtual
///    keyboard appearing) — spatial keying would miss it and the blur
///    would lag by a frame.
///  - An unrelated text input can *appear* at the same screen position
///    after the password field is destroyed — spatial keying would
///    bleed blur onto a non-sensitive control.
///
/// Each scan we re-read the element's `BoundingRectangle` so the blur
/// follows the field as it moves.
#[derive(Clone, Debug)]
struct StickyRegion {
    runtime_id: RuntimeId,
    region: SensitiveRegion,
    /// Last time the element reported `IsPassword=true`. Lets us drop a
    /// sticky entry when the runtime id reappears as a normal `Edit`
    /// without ever flipping back to password — meaning the user
    /// *typed it as plain text* (e.g. into a search box that recycled
    /// the same element). Old confirmations age out.
    last_password_seen: Instant,
}

/// Max age for a sticky entry without any fresh `IsPassword=true`
/// confirmation. Anything older is dropped on the next scan even if
/// the underlying element is still alive — paranoid safety net so a
/// permanently-unmasked input never stays blurred indefinitely.
const STICKY_MAX_AGE: Duration = Duration::from_secs(120);

/// Snapshot of region state published by the background scanner.
/// Accessed by the capture loop under an `RwLock` so reads don't block
/// each other and don't contend with the slow UIA scan thread.
#[derive(Default)]
struct ScanSnapshot {
    regions: Vec<SensitiveRegion>,
    scan_screen_size: Option<(i32, i32)>,
}

/// Stateful privacy filter shared between the capture loop and the
/// DataChannel handler that toggles it.
///
/// **Concurrency model.** The UIA scan (50–500 ms on a busy desktop) is
/// far too slow to run on every captured frame, and even at the 500 ms
/// cadence it would stall the capture loop while it ran. Instead, a
/// dedicated background thread does the scans and pushes the resulting
/// region list into a `RwLock<ScanSnapshot>`. `process_frame` only
/// takes a *read* lock for a few µs and applies the blur — the hot
/// path is decoupled from UIA entirely.
pub struct PrivacyFilter {
    pub enabled: bool,
    /// Pixel radius of the box blur. 15 is enough to make any glyph
    /// unreadable; bump to 25 for very large fonts / accessibility mode.
    pub blur_radius: u32,
    /// Latest scan snapshot, written by the background thread, read by
    /// every captured frame.
    snapshot: Arc<RwLock<ScanSnapshot>>,
    /// `false` once `Drop` runs — signals the scanner thread to exit.
    scanner_alive: Arc<AtomicBool>,
}

impl PrivacyFilter {
    pub fn new() -> Self {
        let snapshot = Arc::new(RwLock::new(ScanSnapshot::default()));
        let scanner_alive = Arc::new(AtomicBool::new(true));

        // Spawn the background scanner exactly once. It owns the
        // sticky cache so process_frame never sees half-updated state.
        spawn_scanner(Arc::clone(&snapshot), Arc::clone(&scanner_alive));

        Self {
            // Privacy by default — only the technician can lift it from
            // the viewer, and only after an explicit click.
            enabled: true,
            blur_radius: DEFAULT_BLUR_RADIUS,
            snapshot,
            scanner_alive,
        }
    }

    /// Apply privacy blur to a captured BGRA frame.
    ///
    /// Hot path: only a *read* lock on the snapshot (no UIA, no
    /// allocation when there are no regions). Returns immediately when
    /// `enabled == false`.
    pub fn process_frame(&mut self, buffer: &mut Vec<u8>, width: u32, height: u32) {
        if !self.enabled {
            return;
        }

        // Read-only snapshot of the current sticky regions.
        let (regions, scan_screen_size) = {
            let snap = match self.snapshot.read() {
                Ok(s) => s,
                // PoisonError — another thread panicked while holding
                // the lock. Treat as "no regions" rather than crash;
                // the next scan will replace it.
                Err(poisoned) => poisoned.into_inner(),
            };
            if snap.regions.is_empty() {
                return;
            }
            (snap.regions.clone(), snap.scan_screen_size)
        };

        // Rescale UIA regions to the buffer if the encoder is fed a
        // downscaled image (4K capture → 1080p stream is common).
        let scaled = match scan_screen_size {
            Some((sw, sh)) if sw > 0 && sh > 0 && (sw as u32 != width || sh as u32 != height) => {
                let sx = width as f32 / sw as f32;
                let sy = height as f32 / sh as f32;
                regions
                    .iter()
                    .map(|r| SensitiveRegion {
                        x: (r.x as f32 * sx) as i32,
                        y: (r.y as f32 * sy) as i32,
                        width: (r.width as f32 * sx).ceil() as i32,
                        height: (r.height as f32 * sy).ceil() as i32,
                    })
                    .collect::<Vec<_>>()
            }
            _ => regions,
        };

        apply_privacy_blur(buffer, width, height, &scaled, self.blur_radius);
    }
}

impl Drop for PrivacyFilter {
    fn drop(&mut self) {
        // Signal the scanner thread to exit at its next iteration.
        // We don't `join` it — the worker sleeps in short ticks and
        // checks the flag, so it'll terminate cleanly on its own.
        self.scanner_alive.store(false, Ordering::Release);
    }
}

impl Default for PrivacyFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Background thread that re-runs the UIA scan every
/// [`DEFAULT_SCAN_INTERVAL_MS`] and publishes the result. Sleeps in
/// 100 ms ticks so it exits promptly when the filter is dropped.
fn spawn_scanner(snapshot: Arc<RwLock<ScanSnapshot>>, alive: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("privacy-filter-scanner".to_string())
        .spawn(move || {
            // Sticky cache lives entirely on this thread — never shared.
            let sticky: Mutex<Vec<StickyRegion>> = Mutex::new(Vec::new());

            // First scan happens immediately so the very first captured
            // frame after enabling the filter already has regions.
            let mut next_scan = Instant::now();

            while alive.load(Ordering::Acquire) {
                let now = Instant::now();
                if now < next_scan {
                    // Coarse-grained sleep so a Drop signal is acted on
                    // within ~100 ms rather than the full scan period.
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                next_scan = now + Duration::from_millis(DEFAULT_SCAN_INTERVAL_MS);

                let (password_uia, edit_uia) = detect_uia_elements();
                let mut sticky_guard = match sticky.lock() {
                    Ok(g) => g,
                    Err(poisoned) => poisoned.into_inner(),
                };

                // ── Refresh / add sticky entries from fresh password hits ──
                for hit in &password_uia {
                    if let Some(existing) = sticky_guard
                        .iter_mut()
                        .find(|s| s.runtime_id == hit.runtime_id)
                    {
                        existing.region = hit.region;
                        existing.last_password_seen = now;
                    } else {
                        sticky_guard.push(StickyRegion {
                            runtime_id: hit.runtime_id.clone(),
                            region: hit.region,
                            last_password_seen: now,
                        });
                    }
                }

                // ── Drop stickies whose runtime id vanished from UIA.
                //       Keep stickies that reappear as a normal Edit
                //       (= "show password" active) and follow their
                //       new bounding rect across scans.
                let password_ids: std::collections::HashSet<&RuntimeId> =
                    password_uia.iter().map(|h| &h.runtime_id).collect();

                sticky_guard.retain_mut(|s| {
                    if password_ids.contains(&s.runtime_id) {
                        return true;
                    }
                    if now.duration_since(s.last_password_seen) > STICKY_MAX_AGE {
                        return false;
                    }
                    if let Some(edit) =
                        edit_uia.iter().find(|e| e.runtime_id == s.runtime_id)
                    {
                        s.region = edit.region;
                        return true;
                    }
                    false
                });

                // ── Publish snapshot for process_frame consumers ──
                let new_regions: Vec<SensitiveRegion> =
                    sticky_guard.iter().map(|s| s.region).collect();
                drop(sticky_guard);

                let new_screen = current_screen_size();
                if let Ok(mut snap) = snapshot.write() {
                    snap.regions = new_regions;
                    snap.scan_screen_size = new_screen;
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|err| {
            // If we can't spawn the worker, fail open (no scan = no blur)
            // rather than crash the agent. Privacy is degraded but the
            // session still runs.
            tracing::warn!("🔒 privacy scanner spawn failed: {err}");
        });
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

/// Pairs a UIA element's stable runtime id with its on-screen rect.
/// Used as the unit returned by [`detect_uia_elements`] so the sticky
/// cache can correlate elements across scans by identity rather than
/// by position.
#[derive(Clone, Debug)]
struct UiaHit {
    runtime_id: RuntimeId,
    region: SensitiveRegion,
}

// ──────────────────────────────────────────────────────────────────────
//                       UI Automation — Windows only
// ──────────────────────────────────────────────────────────────────────

/// Public convenience: returns only the on-screen rects of elements
/// currently reporting `IsPassword=true`. Used by external callers /
/// tests that don't need the sticky-cache plumbing.
pub fn detect_password_fields() -> Vec<SensitiveRegion> {
    detect_uia_elements()
        .0
        .into_iter()
        .map(|h| h.region)
        .collect()
}

/// Scans the desktop tree once and returns two lists of UIA hits:
///  - `.0` : elements whose `IsPassword == true` *right now*,
///  - `.1` : every visible `ControlType == Edit` control.
///
/// Each entry carries both the bounding rect and the element's
/// `RuntimeId`. `PrivacyFilter::refresh_regions` keys the sticky cache
/// on the runtime id so blur follows the *same control* across scans,
/// regardless of position changes or `IsPassword` toggles.
fn detect_uia_elements() -> (Vec<UiaHit>, Vec<UiaHit>) {
    #[cfg(windows)]
    {
        match detect_uia_elements_impl() {
            Ok(pair) => pair,
            Err(err) => {
                tracing::debug!("🔒 UIA scan skipped: {err}");
                (Vec::new(), Vec::new())
            }
        }
    }
    #[cfg(not(windows))]
    {
        (Vec::new(), Vec::new())
    }
}

#[cfg(windows)]
fn detect_uia_elements_impl() -> Result<(Vec<UiaHit>, Vec<UiaHit>), String> {
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
            return Err(format!("CoInitializeEx failed (hr=0x{code:08X})"));
        }

        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("CoCreateInstance(CUIAutomation) failed: {e}"))?;

        let root: IUIAutomationElement = automation
            .GetRootElement()
            .map_err(|e| format!("GetRootElement failed: {e}"))?;

        let mut password_hits = Vec::new();
        let mut edit_hits = Vec::new();

        // ── Pass 1 : IsPassword == true ─────────────────────────────
        // Native Win32, WPF, UWP, Edge/Chrome, Electron — they all
        // advertise IsPassword=true on the masked control. UIA has no
        // dedicated password control type; the type is always `Edit`.
        let pwd_variant = VARIANT::from(true);
        let pwd_condition = automation
            .CreatePropertyCondition(UIA_IsPasswordPropertyId, &pwd_variant)
            .map_err(|e| format!("CreatePropertyCondition(IsPassword) failed: {e}"))?;

        if let Ok(array) = root.FindAll(TreeScope_Subtree, &pwd_condition) {
            let count = array.Length().unwrap_or(0);
            for i in 0..count {
                if let Ok(element) = array.GetElement(i) {
                    if let Some(hit) = element_to_hit(&element) {
                        password_hits.push(hit);
                    }
                }
            }
        }

        // ── Pass 2 : every ControlType == Edit on screen ────────────
        // Used by the sticky-cache layer to know "is *this specific*
        // input still there?" after IsPassword flips to false on
        // show-password. We never blur these unless their runtime id
        // is already in the sticky cache (= confirmed password
        // previously) — see `PrivacyFilter::refresh_regions`.
        let edit_variant = VARIANT::from(UIA_EditControlTypeId.0);
        let edit_condition = automation
            .CreatePropertyCondition(UIA_ControlTypePropertyId, &edit_variant)
            .map_err(|e| format!("CreatePropertyCondition(Edit) failed: {e}"))?;

        if let Ok(array) = root.FindAll(TreeScope_Subtree, &edit_condition) {
            let count = array.Length().unwrap_or(0);
            for i in 0..count {
                if let Ok(element) = array.GetElement(i) {
                    if let Some(hit) = element_to_hit(&element) {
                        edit_hits.push(hit);
                    }
                }
            }
        }

        Ok((password_hits, edit_hits))
    }
}

/// Materializes a UIA element as `(runtime_id, bounding_rect)`. Returns
/// `None` when either piece of metadata is unavailable — we treat such
/// elements as if they didn't exist rather than risk a partial entry.
#[cfg(windows)]
fn element_to_hit(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<UiaHit> {
    unsafe {
        let rect = element.CurrentBoundingRectangle().ok()?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        let runtime_id = extract_runtime_id(element)?;
        if runtime_id.is_empty() {
            return None;
        }
        Some(UiaHit {
            runtime_id,
            region: SensitiveRegion {
                x: rect.left,
                y: rect.top,
                width,
                height,
            },
        })
    }
}

/// Extracts the UIA `RuntimeId` as a `Vec<i32>`. The SAFEARRAY allocated
/// by UIA is destroyed before returning. `None` on any failure (out of
/// the UIA tree, marshalling error) — the caller must treat the
/// element as untrackable in that case.
#[cfg(windows)]
unsafe fn extract_runtime_id(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<RuntimeId> {
    use windows::Win32::System::Ole::{
        SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    };

    let psa = element.GetRuntimeId().ok()?;
    if psa.is_null() {
        return None;
    }

    // RuntimeId is a 1-dimensional SAFEARRAY of i32 (VT_I4). Bounds use
    // 1-based dim index per the OLE convention.
    let lbound = SafeArrayGetLBound(psa, 1).ok()?;
    let ubound = SafeArrayGetUBound(psa, 1).ok()?;
    let count = (ubound - lbound + 1).max(0) as usize;
    if count == 0 {
        let _ = SafeArrayDestroy(psa);
        return None;
    }

    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    if SafeArrayAccessData(psa, &mut data).is_err() || data.is_null() {
        let _ = SafeArrayDestroy(psa);
        return None;
    }
    let slice = std::slice::from_raw_parts(data as *const i32, count);
    let runtime_id: RuntimeId = slice.to_vec();
    // SafeArrayUnaccessData balances the AccessData lock. SafeArrayDestroy
    // frees the descriptor + payload.
    let _ = windows::Win32::System::Ole::SafeArrayUnaccessData(psa);
    let _ = SafeArrayDestroy(psa);
    Some(runtime_id)
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
        // Inject a fake region so we can verify it's *not* applied
        // when enabled=false — bypass the background scanner.
        let mut filter = PrivacyFilter::new();
        filter.enabled = false;
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = vec![SensitiveRegion {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            }];
        }
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn empty_regions_skip_work() {
        // Default snapshot has no regions — process_frame should
        // return without touching the buffer.
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn snapshot_regions_drive_blur() {
        // Inject a region via the snapshot (simulating what the
        // background scanner would write) and verify process_frame
        // actually applies a blur over that area.
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        filter.blur_radius = 4;
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = vec![SensitiveRegion {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            }];
            snap.scan_screen_size = Some((16, 16));
        }
        // Half-black / half-white step image so blur produces a
        // detectable intermediate gray inside the region.
        let mut buf = vec![0u8; 16 * 16 * 4];
        for y in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + x) * 4;
                let v = if x < 8 { 0 } else { 255 };
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
                buf[i + 3] = 255;
            }
        }
        filter.process_frame(&mut buf, 16, 16);
        // Pick a pixel inside the blurred region near the boundary.
        let idx = (6 * 16 + 7) * 4;
        let v = buf[idx];
        assert!(v > 0 && v < 255, "no blur applied inside region (v={v})");
    }
}
