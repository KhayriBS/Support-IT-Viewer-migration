//! Keyboard and mouse input injection.
//!
//! Port of `InputHandler.cs` — replaces `[DllImport("user32.dll")]` P/Invoke
//! with the `windows` crate (`Win32_UI_Input_KeyboardAndMouse`).
//!
//! Accepts the same JSON messages as the C# version:
//!   `{ "type": "mousemove"|"mousedown"|"mouseup"|"click"|"dblclick"|"wheel"|"keydown"|"keyup",
//!      "x": i32, "y": i32, "button": 0|1|2, "deltaY": i32,
//!      "key": "Enter"|…, "keyCode": u8 }`

#[cfg(windows)]
mod win {
    use serde_json::Value;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, mouse_event, MapVirtualKeyW, VkKeyScanW, KEYEVENTF_KEYUP,
        MAPVK_VK_TO_VSC, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
        VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

    // ─── InputHandler ─────────────────────────────────────────────────────────

    pub struct InputHandler;

    impl InputHandler {
        pub fn new() -> Self {
            Self
        }

        /// Dispatches an input JSON message (mirrors `HandleInput(string inputJson)`).
        pub fn handle_input(&self, input_json: &str) {
            let Ok(root) = serde_json::from_str::<Value>(input_json) else {
                return;
            };

            let event_type = root["type"].as_str().unwrap_or("");
            println!("🖱️ Input reçu: {event_type}");

            match event_type {
                "mousemove" | "mouse-move" => self.handle_mouse_move(&root),
                "mousedown" | "mouse-down" => self.handle_mouse_down(&root),
                "mouseup"   | "mouse-up"   => self.handle_mouse_up(&root),
                "click"                     => self.handle_click(&root),
                "dblclick"                  => self.handle_double_click(&root),
                "wheel"                     => self.handle_wheel(&root),
                "keydown"   | "key-down"   => self.handle_key_down(&root),
                "keyup"     | "key-up"     => self.handle_key_up(&root),
                _ => {}
            }
        }

        // ── Mouse helpers ─────────────────────────────────────────────────────

        fn handle_mouse_move(&self, root: &Value) {
            let x = root["x"].as_i64().unwrap_or(0) as i32;
            let y = root["y"].as_i64().unwrap_or(0) as i32;
            unsafe { let _ = SetCursorPos(x, y); }
        }

        fn handle_mouse_down(&self, root: &Value) {
            let x = root["x"].as_i64().unwrap_or(0) as i32;
            let y = root["y"].as_i64().unwrap_or(0) as i32;
            let button = root["button"].as_i64().unwrap_or(0);

            unsafe { let _ = SetCursorPos(x, y); }

            let flag = match button {
                1 => MOUSEEVENTF_MIDDLEDOWN,
                2 => MOUSEEVENTF_RIGHTDOWN,
                _ => MOUSEEVENTF_LEFTDOWN,
            };
            unsafe { mouse_event(flag, 0, 0, 0, 0); }
        }

        fn handle_mouse_up(&self, root: &Value) {
            let x = root["x"].as_i64().unwrap_or(0) as i32;
            let y = root["y"].as_i64().unwrap_or(0) as i32;
            let button = root["button"].as_i64().unwrap_or(0);

            unsafe { let _ = SetCursorPos(x, y); }

            let flag = match button {
                1 => MOUSEEVENTF_MIDDLEUP,
                2 => MOUSEEVENTF_RIGHTUP,
                _ => MOUSEEVENTF_LEFTUP,
            };
            unsafe { mouse_event(flag, 0, 0, 0, 0); }
        }

        fn handle_click(&self, root: &Value) {
            let x = root["x"].as_i64().unwrap_or(0) as i32;
            let y = root["y"].as_i64().unwrap_or(0) as i32;
            let button = root["button"].as_i64().unwrap_or(0);

            unsafe { let _ = SetCursorPos(x, y); }

            let (down, up) = match button {
                1 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                2 => (MOUSEEVENTF_RIGHTDOWN,  MOUSEEVENTF_RIGHTUP),
                _ => (MOUSEEVENTF_LEFTDOWN,   MOUSEEVENTF_LEFTUP),
            };
            unsafe {
                mouse_event(down, 0, 0, 0, 0);
                mouse_event(up,   0, 0, 0, 0);
            }
        }

        fn handle_double_click(&self, root: &Value) {
            self.handle_click(root);
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.handle_click(root);
        }

        fn handle_wheel(&self, root: &Value) {
            let delta_y = root["deltaY"].as_i64().unwrap_or(0);
            // Positive deltaY → scroll down → negative wheel data (Windows convention)
            let wheel_data: i32 = if delta_y > 0 { -120 } else { 120 };
            unsafe { mouse_event(MOUSEEVENTF_WHEEL, 0, 0, wheel_data, 0); }
        }

        // ── Keyboard helpers ──────────────────────────────────────────────────

        fn handle_key_down(&self, root: &Value) {
            if let Some(vk) = self.get_virtual_key(root) {
                let scan = unsafe {
                    MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) as u8
                };
                unsafe { keybd_event(vk.0 as u8, scan, Default::default(), 0); }
            }
        }

        fn handle_key_up(&self, root: &Value) {
            if let Some(vk) = self.get_virtual_key(root) {
                let scan = unsafe {
                    MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) as u8
                };
                unsafe { keybd_event(vk.0 as u8, scan, KEYEVENTF_KEYUP, 0); }
            }
        }

        /// Mirrors `GetKeyCode()` — resolves numeric `keyCode` or string `key`
        /// to a Windows Virtual Key code.
        fn get_virtual_key(&self, root: &Value) -> Option<VIRTUAL_KEY> {
            // 1) Direct numeric keyCode
            if let Some(kc) = root["keyCode"].as_u64() {
                if kc > 0 {
                    return Some(VIRTUAL_KEY(kc as u16));
                }
            }

            // 2) String key name
            if let Some(key) = root["key"].as_str() {
                if key.len() == 1 {
                    let ch = key.chars().next()?;
                    // VkKeyScanW returns (vk | shift_state<<8); we only want the low byte
                    let vk = unsafe { VkKeyScanW(ch as u16) } & 0xFF;
                    if vk > 0 {
                        return Some(VIRTUAL_KEY(vk as u16));
                    }
                }

                return self.map_special_key(key);
            }

            None
        }

        /// Mirrors `MapSpecialKey()`.
        fn map_special_key(&self, key: &str) -> Option<VIRTUAL_KEY> {
            let vk = match key.to_lowercase().as_str() {
                "enter"       => 0x0Du16,
                "tab"         => 0x09,
                "escape"      => 0x1B,
                "backspace"   => 0x08,
                "delete"      => 0x2E,
                "insert"      => 0x2D,
                "home"        => 0x24,
                "end"         => 0x23,
                "pageup"      => 0x21,
                "pagedown"    => 0x22,
                "arrowleft"   => 0x25,
                "arrowup"     => 0x26,
                "arrowright"  => 0x27,
                "arrowdown"   => 0x28,
                "shift"       => 0x10,
                "control"     => 0x11,
                "alt"         => 0x12,
                "capslock"    => 0x14,
                "space"       => 0x20,
                "f1"  => 0x70, "f2"  => 0x71, "f3"  => 0x72,
                "f4"  => 0x73, "f5"  => 0x74, "f6"  => 0x75,
                "f7"  => 0x76, "f8"  => 0x77, "f9"  => 0x78,
                "f10" => 0x79, "f11" => 0x7A, "f12" => 0x7B,
                _ => return None,
            };
            Some(VIRTUAL_KEY(vk))
        }
    }

    impl Default for InputHandler {
        fn default() -> Self { Self::new() }
    }
}

// ─── Public re-export (no-op stub on non-Windows) ────────────────────────────

#[cfg(windows)]
pub use win::InputHandler;

#[cfg(not(windows))]
pub struct InputHandler;

#[cfg(not(windows))]
impl InputHandler {
    pub fn new() -> Self { Self }
    /// No-op on non-Windows platforms.
    pub fn handle_input(&self, _input_json: &str) {}
}

#[cfg(not(windows))]
impl Default for InputHandler {
    fn default() -> Self { Self::new() }
}
