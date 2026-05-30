// Identifiant matériel stable de la machine.

use std::process::Command;


pub fn get_hardware_serial() -> String {
    if let Some(serial) = read_via_wmic() {
        tracing::debug!("hardware serial via wmic: {serial}");
        return serial;
    }

    if let Some(serial) = read_via_powershell() {
        tracing::debug!("hardware serial via powershell: {serial}");
        return serial;
    }

    let fallback = hostname_fallback();
    tracing::warn!(
        "Aucun serial BIOS lisible — fallback hostname ({fallback}). \
         Vérifiez que l'agent tourne avec des droits suffisants."
    );
    fallback
}

#[cfg(target_os = "windows")]
fn read_via_wmic() -> Option<String> {
 
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let output = Command::new("wmic")
        .args(["bios", "get", "serialnumber"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_keyed_output(&String::from_utf8_lossy(&output.stdout), "SerialNumber")
}

#[cfg(target_os = "windows")]
fn read_via_powershell() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance -ClassName Win32_BIOS).SerialNumber",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    sanitize(raw)
}

#[cfg(not(target_os = "windows"))]
fn read_via_wmic() -> Option<String> {
    None
}

#[cfg(not(target_os = "windows"))]
fn read_via_powershell() -> Option<String> {
    None
}

fn parse_keyed_output(stdout: &str, header: &str) -> Option<String> {
 
    let mut header_seen = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !header_seen {
            if trimmed.eq_ignore_ascii_case(header) {
                header_seen = true;
            }
            continue;
        }
        if let Some(value) = sanitize(trimmed.to_string()) {
            return Some(value);
        }
    }
    None
}

fn sanitize(raw: String) -> Option<String> {
    let cleaned = raw.trim().to_string();
    // Valeurs que wmic peut retourner quand le BIOS n'expose rien d'utile.
    let blacklist = [
        "",
        "to be filled by o.e.m.",
        "to be filled by oem",
        "default string",
        "system serial number",
        "none",
        "0",
    ];
    if blacklist.iter().any(|b| cleaned.eq_ignore_ascii_case(b)) {
        return None;
    }
    Some(cleaned)
}

fn hostname_fallback() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
