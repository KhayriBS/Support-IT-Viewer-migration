//! Privacy filter — detects password fields via Windows UI Automation and blurs them on the BGRA buffer before H.264 encoding.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SensitiveRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

const DEFAULT_SCAN_INTERVAL_MS: u64 = 500;
const GLOBAL_SCAN_EVERY_N_ITERATIONS: u32 = 10;
pub const DEFAULT_BLUR_RADIUS: u32 = 15;
const MAX_REGION_PIXELS: u64 = 8_192 * 8_192;

type RuntimeId = Vec<i32>;

#[derive(Clone, Debug)]
struct StickyRegion {
    runtime_id: RuntimeId,
    region: SensitiveRegion,
    last_password_seen: Instant,
}

const STICKY_MAX_AGE: Duration = Duration::from_secs(3_600);
const SCANNER_HEARTBEAT_STALE_MS: u64 = 5_000;
const SCANNER_HEARTBEAT_DEAD_MS: u64 = 15_000;

#[derive(Default)]
struct ScanSnapshot {
    regions: Arc<Vec<SensitiveRegion>>,
    scan_screen_size: Option<(i32, i32)>,
}

#[derive(Default)]
struct ScannerHealth {
    last_heartbeat: Option<Instant>,
    scan_count: u64,
    fail_count: u64,
    last_scan_duration_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScannerStatus {
    Ok,
    Degraded,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub struct PrivacyFilterStats {
    pub enabled: bool,
    pub regions_count: usize,
    pub last_scan_duration_ms: u32,
    pub last_scan_age_ms: u64,
    pub scan_count: u64,
    pub fail_count: u64,
    pub scanner_status: ScannerStatus,
}

pub struct PrivacyFilter {
    pub enabled: bool,
    pub blur_radius: u32,
    snapshot: Arc<RwLock<ScanSnapshot>>,
    scanner_alive: Arc<AtomicBool>,
    scanner_spawned: bool,
    health: Arc<RwLock<ScannerHealth>>,
    blur_tmp_buffer: Vec<u8>,
}

impl PrivacyFilter {
    pub fn new() -> Self {
        let snapshot = Arc::new(RwLock::new(ScanSnapshot::default()));
        let scanner_alive = Arc::new(AtomicBool::new(true));
        let health = Arc::new(RwLock::new(ScannerHealth::default()));

        // Synchronous initial scan so the first captured frame already has regions.
        run_initial_scan(&snapshot, &health);

        let scanner_spawned = spawn_scanner(
            Arc::clone(&snapshot),
            Arc::clone(&scanner_alive),
            Arc::clone(&health),
        );

        if !scanner_spawned {
            tracing::warn!("🔒 privacy filter running in BLACKOUT mode — scanner thread failed to start");
        }

        Self {
            enabled: true,
            blur_radius: DEFAULT_BLUR_RADIUS,
            snapshot,
            scanner_alive,
            scanner_spawned,
            health,
            blur_tmp_buffer: Vec::new(),
        }
    }

    pub fn process_frame(&mut self, buffer: &mut Vec<u8>, width: u32, height: u32) {
        if !self.enabled {
            return;
        }

        // Fail-closed: scanner dead → black out frame instead of leaking unfiltered pixels.
        if self.scanner_status() == ScannerStatus::Failed {
            black_out_buffer(buffer, width, height);
            return;
        }

        let (regions_arc, scan_screen_size) = {
            let snap = match self.snapshot.read() {
                Ok(s) => s,
                Err(poisoned) => poisoned.into_inner(),
            };
            if snap.regions.is_empty() {
                return;
            }
            (Arc::clone(&snap.regions), snap.scan_screen_size)
        };

        // Rescale only when capture resolution differs from UIA scan resolution.
        let scaled_storage: Option<Vec<SensitiveRegion>> = match scan_screen_size {
            Some((sw, sh)) if sw > 0 && sh > 0 && (sw as u32 != width || sh as u32 != height) => {
                let sx = width as f32 / sw as f32;
                let sy = height as f32 / sh as f32;
                Some(
                    regions_arc
                        .iter()
                        .map(|r| SensitiveRegion {
                            x: ((r.x as f32) * sx) as i32,
                            y: ((r.y as f32) * sy) as i32,
                            width: ((r.width as f32) * sx).ceil() as i32,
                            height: ((r.height as f32) * sy).ceil() as i32,
                        })
                        .collect(),
                )
            }
            _ => None,
        };
        let regions: &[SensitiveRegion] = match scaled_storage.as_deref() {
            Some(s) => s,
            None => regions_arc.as_slice(),
        };

        for region in regions {
            blur_region_with_scratch(
                buffer,
                width,
                height,
                region,
                self.blur_radius,
                &mut self.blur_tmp_buffer,
            );
        }
    }

    pub fn stats(&self) -> PrivacyFilterStats {
        let regions_count = match self.snapshot.read() {
            Ok(s) => s.regions.len(),
            Err(poisoned) => poisoned.into_inner().regions.len(),
        };
        let (last_scan_age_ms, last_scan_duration_ms, scan_count, fail_count) =
            match self.health.read() {
                Ok(h) => (
                    h.last_heartbeat.map(|t| t.elapsed().as_millis() as u64).unwrap_or(u64::MAX),
                    h.last_scan_duration_ms,
                    h.scan_count,
                    h.fail_count,
                ),
                Err(poisoned) => {
                    let h = poisoned.into_inner();
                    (
                        h.last_heartbeat.map(|t| t.elapsed().as_millis() as u64).unwrap_or(u64::MAX),
                        h.last_scan_duration_ms,
                        h.scan_count,
                        h.fail_count,
                    )
                }
            };
        PrivacyFilterStats {
            enabled: self.enabled,
            regions_count,
            last_scan_duration_ms,
            last_scan_age_ms,
            scan_count,
            fail_count,
            scanner_status: self.scanner_status(),
        }
    }

    fn scanner_status(&self) -> ScannerStatus {
        if !self.scanner_spawned {
            return ScannerStatus::Failed;
        }
        let last_heartbeat = match self.health.read() {
            Ok(h) => h.last_heartbeat,
            Err(poisoned) => poisoned.into_inner().last_heartbeat,
        };
        match last_heartbeat {
            None => ScannerStatus::Degraded,
            Some(ts) => {
                let age_ms = ts.elapsed().as_millis() as u64;
                if age_ms >= SCANNER_HEARTBEAT_DEAD_MS {
                    ScannerStatus::Failed
                } else if age_ms >= SCANNER_HEARTBEAT_STALE_MS {
                    ScannerStatus::Degraded
                } else {
                    ScannerStatus::Ok
                }
            }
        }
    }
}

impl Drop for PrivacyFilter {
    fn drop(&mut self) {
        self.scanner_alive.store(false, Ordering::Release);
    }
}

impl Default for PrivacyFilter {
    fn default() -> Self {
        Self::new()
    }
}

// Zero out RGB, keep alpha at 0xFF — fail-closed signal to the technician.
fn black_out_buffer(buffer: &mut [u8], width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let stride = (width as usize).saturating_mul(4);
    let expected_len = stride.saturating_mul(height as usize);
    let len = buffer.len().min(expected_len);
    for chunk in buffer[..len].chunks_exact_mut(4) {
        chunk[0] = 0;
        chunk[1] = 0;
        chunk[2] = 0;
        chunk[3] = 0xFF;
    }
}

fn run_initial_scan(
    snapshot: &Arc<RwLock<ScanSnapshot>>,
    health: &Arc<RwLock<ScannerHealth>>,
) {
    let started = Instant::now();
    let (password_hits, _edit_hits) = detect_uia_elements(ScanScope::Full);
    let elapsed_ms = started.elapsed().as_millis() as u32;

    let regions: Vec<SensitiveRegion> = password_hits.into_iter().map(|h| h.region).collect();
    let screen = current_capture_screen_size();

    if let Ok(mut snap) = snapshot.write() {
        snap.regions = Arc::new(regions);
        snap.scan_screen_size = screen;
    }
    if let Ok(mut h) = health.write() {
        h.last_heartbeat = Some(Instant::now());
        h.scan_count = 1;
        h.last_scan_duration_ms = elapsed_ms;
    }
}

fn spawn_scanner(
    snapshot: Arc<RwLock<ScanSnapshot>>,
    alive: Arc<AtomicBool>,
    health: Arc<RwLock<ScannerHealth>>,
) -> bool {
    let result = thread::Builder::new()
        .name("privacy-filter-scanner".to_string())
        .spawn(move || {
            let mut sticky: Vec<StickyRegion> = Vec::new();

            // Match UIA coords with physical-pixel DXGI buffer on HiDPI monitors.
            #[cfg(windows)]
            set_thread_dpi_aware();

            let mut iteration: u32 = 0;
            let mut next_scan = Instant::now();

            while alive.load(Ordering::Acquire) {
                let now = Instant::now();
                if now < next_scan {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                next_scan = now + Duration::from_millis(DEFAULT_SCAN_INTERVAL_MS);

                // Heartbeat before the slow UIA call so hangs surface as degraded.
                if let Ok(mut h) = health.write() {
                    h.last_heartbeat = Some(Instant::now());
                }

                let scope = if iteration % GLOBAL_SCAN_EVERY_N_ITERATIONS == 0 {
                    ScanScope::Full
                } else {
                    ScanScope::Foreground
                };
                iteration = iteration.wrapping_add(1);

                let scan_started = Instant::now();
                let (password_uia, edit_uia) = detect_uia_elements(scope);
                let scan_elapsed = scan_started.elapsed();
                let scan_ok = !(password_uia.is_empty() && edit_uia.is_empty())
                    || scope == ScanScope::Foreground;

                for hit in &password_uia {
                    if let Some(existing) =
                        sticky.iter_mut().find(|s| s.runtime_id == hit.runtime_id)
                    {
                        existing.region = hit.region;
                        existing.last_password_seen = now;
                    } else {
                        sticky.push(StickyRegion {
                            runtime_id: hit.runtime_id.clone(),
                            region: hit.region,
                            last_password_seen: now,
                        });
                    }
                }

                let password_ids: std::collections::HashSet<&RuntimeId> =
                    password_uia.iter().map(|h| &h.runtime_id).collect();

                sticky.retain_mut(|s| {
                    if password_ids.contains(&s.runtime_id) {
                        return true;
                    }
                    if now.duration_since(s.last_password_seen) > STICKY_MAX_AGE {
                        return false;
                    }
                    if let Some(edit) = edit_uia.iter().find(|e| e.runtime_id == s.runtime_id) {
                        s.region = edit.region;
                        return true;
                    }
                    // Foreground scans don't enumerate background windows — only Full scans can confirm a missing element.
                    match scope {
                        ScanScope::Full => false,
                        ScanScope::Foreground => true,
                    }
                });

                let new_regions: Vec<SensitiveRegion> =
                    sticky.iter().map(|s| s.region).collect();

                let new_screen = current_capture_screen_size();
                if let Ok(mut snap) = snapshot.write() {
                    snap.regions = Arc::new(new_regions);
                    snap.scan_screen_size = new_screen;
                }

                if let Ok(mut h) = health.write() {
                    h.last_heartbeat = Some(Instant::now());
                    h.scan_count = h.scan_count.saturating_add(1);
                    h.last_scan_duration_ms = scan_elapsed.as_millis() as u32;
                    if !scan_ok {
                        h.fail_count = h.fail_count.saturating_add(1);
                    }
                }
            }
        });

    match result {
        Ok(_join_handle) => true,
        Err(err) => {
            tracing::warn!("🔒 privacy scanner spawn failed: {err}");
            false
        }
    }
}

pub fn apply_privacy_blur(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    regions: &[SensitiveRegion],
    radius: u32,
) {
    if width == 0 || height == 0 || buffer.is_empty() || regions.is_empty() {
        return;
    }
    let mut scratch: Vec<u8> = Vec::new();
    for region in regions {
        blur_region_with_scratch(buffer, width, height, region, radius, &mut scratch);
    }
}

pub fn blur_region(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    region: &SensitiveRegion,
    radius: u32,
) {
    let mut scratch: Vec<u8> = Vec::new();
    blur_region_with_scratch(buffer, width, height, region, radius, &mut scratch);
}

fn blur_region_with_scratch(
    buffer: &mut [u8],
    width: u32,
    height: u32,
    region: &SensitiveRegion,
    radius: u32,
    scratch: &mut Vec<u8>,
) {
    if width == 0 || height == 0 || radius == 0 {
        return;
    }

    let stride = (width as usize).saturating_mul(4);
    let expected_len = stride.saturating_mul(height as usize);
    if buffer.len() < expected_len {
        return;
    }

    // Saturating arithmetic — UIA can hand us absurd rectangles, plain i32 overflow would wrap.
    let x0 = region.x.max(0) as u32;
    let y0 = region.y.max(0) as u32;
    let x1_signed = region.x.saturating_add(region.width).max(0);
    let y1_signed = region.y.saturating_add(region.height).max(0);
    let x1 = (x1_signed as u32).min(width);
    let y1 = (y1_signed as u32).min(height);
    let x0 = x0.min(width);
    let y0 = y0.min(height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let region_w = (x1 - x0) as usize;
    let region_h = (y1 - y0) as usize;

    // OOM guard against malformed UIA rects.
    if (region_w as u64).saturating_mul(region_h as u64) > MAX_REGION_PIXELS {
        tracing::warn!(
            "🔒 privacy blur: region {}×{} exceeds {} pixel cap — skipped",
            region_w,
            region_h,
            MAX_REGION_PIXELS
        );
        return;
    }

    let r = radius as i32;

    let scratch_len = region_w.saturating_mul(region_h).saturating_mul(3);
    if scratch_len == 0 {
        return;
    }
    if scratch.len() < scratch_len {
        scratch.resize(scratch_len, 0);
    } else {
        for byte in &mut scratch[..scratch_len] {
            *byte = 0;
        }
    }
    let tmp = &mut scratch[..scratch_len];

    // Pass 1: horizontal box blur.
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

    // Pass 2: vertical box blur, write back into buffer (alpha untouched).
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
        }
    }
}

// Virtual screen union — keeps rescale correct on multi-monitor captures.
fn current_capture_screen_size() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
        };
        unsafe {
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if vw > 0 && vh > 0 {
                return Some((vw, vh));
            }
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

#[cfg(windows)]
fn set_thread_dpi_aware() {
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[derive(Clone, Debug)]
struct UiaHit {
    runtime_id: RuntimeId,
    region: SensitiveRegion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanScope {
    Full,
    Foreground,
}

pub fn detect_password_fields() -> Vec<SensitiveRegion> {
    detect_uia_elements(ScanScope::Full)
        .0
        .into_iter()
        .map(|h| h.region)
        .collect()
}

fn detect_uia_elements(scope: ScanScope) -> (Vec<UiaHit>, Vec<UiaHit>) {
    #[cfg(windows)]
    {
        match detect_uia_elements_impl(scope) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::debug!("🔒 UIA scan skipped ({scope:?}): {err}");
                (Vec::new(), Vec::new())
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = scope;
        (Vec::new(), Vec::new())
    }
}

#[cfg(windows)]
fn detect_uia_elements_impl(scope: ScanScope) -> Result<(Vec<UiaHit>, Vec<UiaHit>), String> {
    use windows::core::VARIANT;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Subtree,
        UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_IsPasswordPropertyId,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    unsafe {
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

        // Foreground scope falls back to desktop root if no foreground window.
        let root: IUIAutomationElement = match scope {
            ScanScope::Full => automation
                .GetRootElement()
                .map_err(|e| format!("GetRootElement failed: {e}"))?,
            ScanScope::Foreground => {
                let hwnd: HWND = GetForegroundWindow();
                if hwnd.0.is_null() {
                    automation
                        .GetRootElement()
                        .map_err(|e| format!("GetRootElement failed: {e}"))?
                } else {
                    match automation.ElementFromHandle(hwnd) {
                        Ok(el) => el,
                        Err(_) => automation
                            .GetRootElement()
                            .map_err(|e| format!("GetRootElement failed: {e}"))?,
                    }
                }
            }
        };

        let mut password_hits = Vec::new();
        let mut edit_hits = Vec::new();

        // Pass 1: IsPassword == true (Win32, WPF, UWP, browsers, Electron).
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

        // Pass 2: every Edit control — feeds the sticky cache for "show password" tracking.
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

    // 1-based dim index per OLE convention.
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
    let _ = windows::Win32::System::Ole::SafeArrayUnaccessData(psa);
    let _ = SafeArrayDestroy(psa);
    Some(runtime_id)
}

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
        assert_eq!(buf[0..4], [10, 20, 30, 255]);
    }

    #[test]
    fn blur_smooths_step_edge() {
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
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = Arc::new(vec![SensitiveRegion {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            }]);
        }
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn empty_regions_skip_work() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        {
            let mut h = filter.health.write().unwrap();
            h.last_heartbeat = Some(Instant::now());
        }
        let mut buf = solid_buffer(16, 16, [42, 42, 42, 255]);
        let before = buf.clone();
        filter.process_frame(&mut buf, 16, 16);
        assert_eq!(buf, before);
    }

    #[test]
    fn snapshot_regions_drive_blur() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        filter.blur_radius = 4;
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = Arc::new(vec![SensitiveRegion {
                x: 4,
                y: 4,
                width: 8,
                height: 8,
            }]);
            snap.scan_screen_size = Some((16, 16));
        }
        {
            let mut h = filter.health.write().unwrap();
            h.last_heartbeat = Some(Instant::now());
        }
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
        let idx = (6 * 16 + 7) * 4;
        let v = buf[idx];
        assert!(v > 0 && v < 255, "no blur applied inside region (v={v})");
    }

    #[test]
    fn blur_caps_oversized_region() {
        let mut buf = solid_buffer(16, 16, [10, 20, 30, 255]);
        let before = buf.clone();
        let region = SensitiveRegion {
            x: 0,
            y: 0,
            width: i32::MAX,
            height: i32::MAX,
        };
        blur_region(&mut buf, 16, 16, &region, 4);
        assert_eq!(buf.len(), before.len());
    }

    #[test]
    fn blur_handles_negative_origin() {
        let mut buf = solid_buffer(16, 16, [200, 200, 200, 255]);
        let region = SensitiveRegion {
            x: -8,
            y: -8,
            width: 16,
            height: 16,
        };
        blur_region(&mut buf, 16, 16, &region, 3);
    }

    #[test]
    fn black_out_zeroes_rgb_keeps_alpha() {
        let mut buf = solid_buffer(4, 4, [10, 20, 30, 255]);
        black_out_buffer(&mut buf, 4, 4);
        for px in buf.chunks_exact(4) {
            assert_eq!(px[0], 0);
            assert_eq!(px[1], 0);
            assert_eq!(px[2], 0);
            assert_eq!(px[3], 0xFF);
        }
    }

    #[test]
    fn fail_closed_blacks_out_when_scanner_dead() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        {
            let mut h = filter.health.write().unwrap();
            h.last_heartbeat = Some(
                Instant::now() - Duration::from_millis(SCANNER_HEARTBEAT_DEAD_MS + 1_000),
            );
        }
        let mut buf = solid_buffer(8, 8, [99, 99, 99, 255]);
        filter.process_frame(&mut buf, 8, 8);
        for px in buf.chunks_exact(4) {
            assert_eq!((px[0], px[1], px[2], px[3]), (0, 0, 0, 0xFF));
        }
    }

    #[test]
    fn fail_closed_when_scanner_never_spawned() {
        let mut filter = PrivacyFilter::new();
        filter.scanner_spawned = false;
        filter.enabled = true;
        let mut buf = solid_buffer(8, 8, [7, 8, 9, 255]);
        filter.process_frame(&mut buf, 8, 8);
        for px in buf.chunks_exact(4) {
            assert_eq!((px[0], px[1], px[2], px[3]), (0, 0, 0, 0xFF));
        }
        assert_eq!(filter.stats().scanner_status, ScannerStatus::Failed);
    }

    #[test]
    fn stats_report_regions_count() {
        let filter = PrivacyFilter::new();
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = Arc::new(vec![
                SensitiveRegion { x: 0, y: 0, width: 4, height: 4 },
                SensitiveRegion { x: 4, y: 4, width: 4, height: 4 },
                SensitiveRegion { x: 8, y: 8, width: 4, height: 4 },
            ]);
        }
        let stats = filter.stats();
        assert_eq!(stats.regions_count, 3);
        assert!(stats.enabled);
    }

    #[test]
    fn rescale_maps_uia_coords_to_buffer() {
        let mut filter = PrivacyFilter::new();
        filter.enabled = true;
        filter.blur_radius = 3;
        {
            let mut snap = filter.snapshot.write().unwrap();
            snap.regions = Arc::new(vec![SensitiveRegion {
                x: 14,
                y: 14,
                width: 8,
                height: 8,
            }]);
            snap.scan_screen_size = Some((32, 32));
        }
        {
            let mut h = filter.health.write().unwrap();
            h.last_heartbeat = Some(Instant::now());
        }
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

        let idx = (9 * 16 + 8) * 4;
        let v = buf[idx];
        assert!(
            v > 0 && v < 255,
            "rescaled region not blurred (v={v}) — coord mapping broken"
        );
    }

    #[test]
    fn apply_privacy_blur_skips_empty_regions() {
        let mut buf = solid_buffer(8, 8, [1, 2, 3, 4]);
        let before = buf.clone();
        apply_privacy_blur(&mut buf, 8, 8, &[], 5);
        assert_eq!(buf, before);
    }

    #[test]
    fn apply_privacy_blur_reuses_scratch_across_regions() {
        let mut buf = vec![0u8; 32 * 32 * 4];
        for i in 0..buf.len() {
            buf[i] = (i % 251) as u8;
        }
        let regions = [
            SensitiveRegion { x: 0, y: 0, width: 8, height: 8 },
            SensitiveRegion { x: 10, y: 10, width: 8, height: 8 },
            SensitiveRegion { x: 20, y: 20, width: 8, height: 8 },
        ];
        apply_privacy_blur(&mut buf, 32, 32, &regions, 3);
        for r in &regions {
            let cx = (r.x + r.width / 2) as usize;
            let cy = (r.y + r.height / 2) as usize;
            let idx = (cy * 32 + cx) * 4;
            let original = ((cy * 32 + cx) * 4) % 251;
            assert!(
                (buf[idx] as usize) != original
                    || (buf[idx + 1] as usize) != original
                    || (buf[idx + 2] as usize) != original,
                "region at ({}, {}) appears unblurred",
                cx,
                cy
            );
        }
    }
}
