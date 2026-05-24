//! GDI BitBlt capture backend — fallback universel.
//!
//! Utilisé comme dernier recours quand DXGI Desktop Duplication ET Windows Graphics Capture sont indisponibles (cas rare sur Windows 8.1+). --- IGNORE ---

#[cfg(windows)]
use super::ScreenCapturer;
#[cfg(windows)]
use crate::agent::desktop_duplication::{DesktopFrame, GdiCapturer};

#[cfg(windows)]
pub struct GdiBacked {
    inner: GdiCapturer,
}

#[cfg(windows)]
impl GdiBacked {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            inner: GdiCapturer::new()?,
        })
    }
}

#[cfg(windows)]
impl ScreenCapturer for GdiBacked {
    fn capture_next_frame(
        &mut self,
        _timeout_ms: u32,
    ) -> Result<Option<DesktopFrame>, String> {
        // GDI est synchrone : pas de notion de timeout côté API. On capture
        // immédiatement et retourne. Le throttling FPS est fait en amont.
        self.inner.capture_frame().map(Some)
    }
}

#[cfg(not(windows))]
use super::ScreenCapturer;
#[cfg(not(windows))]
use crate::agent::desktop_duplication::DesktopFrame;

#[cfg(not(windows))]
pub struct GdiBacked;

#[cfg(not(windows))]
impl GdiBacked {
    pub fn new() -> Result<Self, String> {
        Err("GDI capture is only available on Windows".to_string())
    }
}

#[cfg(not(windows))]
impl ScreenCapturer for GdiBacked {
    fn capture_next_frame(
        &mut self,
        _timeout_ms: u32,
    ) -> Result<Option<DesktopFrame>, String> {
        Err("GDI capture is only available on Windows".to_string())
    }
}
