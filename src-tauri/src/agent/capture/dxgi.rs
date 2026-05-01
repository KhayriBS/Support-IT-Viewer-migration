use super::ScreenCapturer;
use crate::agent::desktop_duplication::{DesktopFrame, DxgiDesktopDuplicator};

pub struct DxgiCapturer {
    inner: DxgiDesktopDuplicator,
}

impl DxgiCapturer {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            inner: DxgiDesktopDuplicator::new()?,
        })
    }
}

impl ScreenCapturer for DxgiCapturer {
    fn capture_next_frame(&mut self, timeout_ms: u32) -> Result<Option<DesktopFrame>, String> {
        self.inner.capture_next_frame(timeout_ms)
    }
}
