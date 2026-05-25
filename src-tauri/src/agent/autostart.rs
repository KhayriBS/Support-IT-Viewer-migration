//! Windows autostart — registers the agent under HKCU\…\Run so it
//! launches with the current user's session, exactly like AnyDesk /
//! Chrome Remote Desktop.
//!
//! We write directly to the registry via `windows-rs` instead of using
//! `tauri-plugin-autostart` because we need to control two things the
//! plugin doesn't expose cleanly:
//!  - the *exact* exe path (`std::env::current_exe()` resolved once),
//!  - the `--minimized` argument so the boot launch goes straight to
//!    the system tray with no visible window.
//!
//! All functions degrade gracefully — registry access can fail under
//! GPO restrictions, redirected hives, or non-Windows builds. We never
//! panic; the UI just reflects the last known state.

#[cfg(windows)]
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Value name written under HKCU\…\Run. Must match across enable / check
/// / disable — change it here, change it everywhere.
pub const AUTOSTART_VALUE_NAME: &str = "LumiereAgent";

/// Argument the registered command line passes back to the binary.
/// `main.rs::setup` looks for this exact string and hides the window.
pub const AUTOSTART_FLAG: &str = "--minimized";

// ──────────────────────────────────────────────────────────────────────
//                              Public API
// ──────────────────────────────────────────────────────────────────────

/// Returns `true` when the autostart value exists under HKCU\…\Run.
/// Returns `false` on any failure (key missing, permission denied,
/// non-Windows build) — never propagates an error to the UI.
pub fn is_autostart_enabled() -> bool {
    #[cfg(windows)]
    {
        read_autostart_value().is_some()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Enables autostart by writing `"<exe path>" --minimized` to
/// HKCU\…\Run\LumiereAgent (REG_SZ).
pub fn enable_autostart() -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map_err(|e| format!("current_exe failed: {e}"))?;
        let exe_str = exe
            .to_str()
            .ok_or_else(|| "current_exe path is not valid UTF-16".to_string())?;
        // Quote the path so spaces in "Program Files" don't break the
        // CreateProcess invocation Windows uses on login.
        let command_line = format!("\"{exe_str}\" {AUTOSTART_FLAG}");
        write_autostart_value(&command_line)
    }
    #[cfg(not(windows))]
    {
        Err("Autostart is only supported on Windows".to_string())
    }
}

/// Removes the autostart value from HKCU\…\Run, if present. A missing
/// value is treated as success (idempotent disable).
pub fn disable_autostart() -> Result<(), String> {
    #[cfg(windows)]
    {
        delete_autostart_value()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

/// Flips autostart and returns the new state (`true` = now enabled).
/// Idempotent: re-enabling overwrites the existing value, re-disabling
/// returns `Ok(false)`.
pub fn toggle_autostart() -> Result<bool, String> {
    if is_autostart_enabled() {
        disable_autostart()?;
        Ok(false)
    } else {
        enable_autostart()?;
        Ok(true)
    }
}

// ──────────────────────────────────────────────────────────────────────
//                          Windows registry impl
// ──────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read the current REG_SZ value, if any. Returns `None` on every kind
/// of failure (key missing, value missing, wrong type, permission).
#[cfg(windows)]
fn read_autostart_value() -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_VALUE_TYPE,
    };

    unsafe {
        let mut hkey = HKEY::default();
        let key_w = to_wide(RUN_KEY_PATH);
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if open != ERROR_SUCCESS {
            return None;
        }

        // First call sizes the buffer, second call reads.
        let value_w = to_wide(AUTOSTART_VALUE_NAME);
        let mut data_len: u32 = 0;
        let mut value_type = REG_VALUE_TYPE::default();
        let size_call = RegQueryValueExW(
            hkey,
            PCWSTR(value_w.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut data_len),
        );

        if size_call != ERROR_SUCCESS || data_len == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }

        let mut buf = vec![0u8; data_len as usize];
        let read_call = RegQueryValueExW(
            hkey,
            PCWSTR(value_w.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr()),
            Some(&mut data_len),
        );
        let _ = RegCloseKey(hkey);

        if read_call != ERROR_SUCCESS {
            return None;
        }

        // Reinterpret the byte buffer as UTF-16. Strip trailing NULs.
        let u16_len = (data_len as usize) / 2;
        let u16_slice: Vec<u16> = (0..u16_len)
            .map(|i| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]))
            .collect();
        let end = u16_slice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(u16_slice.len());
        Some(String::from_utf16_lossy(&u16_slice[..end]))
    }
}

#[cfg(windows)]
fn write_autostart_value(command_line: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
        REG_SZ,
    };

    unsafe {
        let mut hkey = HKEY::default();
        let key_w = to_wide(RUN_KEY_PATH);
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            0,
            KEY_WRITE,
            &mut hkey,
        );
        if open != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW(Run) failed: WIN32_ERROR({})", open.0));
        }

        // REG_SZ is UTF-16, NUL-terminated, length in *bytes*.
        let value_w = to_wide(AUTOSTART_VALUE_NAME);
        let data_w = to_wide(command_line);
        let data_bytes: &[u8] = std::slice::from_raw_parts(
            data_w.as_ptr() as *const u8,
            data_w.len() * 2,
        );

        let result = RegSetValueExW(
            hkey,
            PCWSTR(value_w.as_ptr()),
            0,
            REG_SZ,
            Some(data_bytes),
        );
        let _ = RegCloseKey(hkey);

        if result != ERROR_SUCCESS {
            return Err(format!(
                "RegSetValueExW(LumiereAgent) failed: WIN32_ERROR({})",
                result.0
            ));
        }
        tracing::info!("🟢 Autostart enabled: {command_line}");
        Ok(())
    }
}

#[cfg(windows)]
fn delete_autostart_value() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
    };

    unsafe {
        let mut hkey = HKEY::default();
        let key_w = to_wide(RUN_KEY_PATH);
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            0,
            KEY_WRITE,
            &mut hkey,
        );
        if open != ERROR_SUCCESS {
            return Err(format!("RegOpenKeyExW(Run) failed: WIN32_ERROR({})", open.0));
        }

        let value_w = to_wide(AUTOSTART_VALUE_NAME);
        let result = RegDeleteValueW(hkey, PCWSTR(value_w.as_ptr()));
        let _ = RegCloseKey(hkey);

        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            tracing::info!("🔴 Autostart disabled");
            Ok(())
        } else {
            Err(format!(
                "RegDeleteValueW failed: WIN32_ERROR({})",
                result.0
            ))
        }
    }
}
