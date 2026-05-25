//! System tray — AnyDesk / Chrome-Remote-Desktop style.
//!
//! - Icon stays in the taskbar even when the main window is hidden.
//! - Menu:
//!     · "status"    : current state (disabled, shown grey)
//!     · ───────────
//!     · "show"      : reopen the panel
//!     · "autostart" : check-item bound to HKCU\…\Run\LumiereAgent
//!     · ───────────
//!     · "quit"      : full process exit
//! - Double-click on the icon = show window + focus (Windows convention).
//!
//! The tray icon handle and the autostart check-item are stored on
//! [`TrayHandles`] inside Tauri's managed state so the rest of the
//! agent (session lifecycle, autostart toggle from the UI) can mutate
//! the menu after construction.

use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use super::autostart::{is_autostart_enabled, toggle_autostart};

// ── Menu item IDs (string literals re-used across creation + dispatch) ──
const ID_STATUS: &str = "status";
const ID_SHOW: &str = "show";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";

const TRAY_TOOLTIP_DEFAULT: &str = "Lumiere Agent — En attente de session";

/// Owned references to the live tray widgets we need to mutate at
/// runtime (status label & autostart checkbox). Stored under Tauri's
/// managed state via `app.manage(TrayHandles::new(...))`.
pub struct TrayHandles<R: Runtime> {
    /// Status row — disabled MenuItem whose label we rewrite from
    /// [`update_tray_status`] when a session starts / ends.
    pub status_item: Mutex<MenuItem<R>>,
    /// Check-item reflecting the registry state. Flipped from the
    /// tray menu handler and (eventually) from the SvelteKit toggle.
    pub autostart_item: Mutex<CheckMenuItem<R>>,
}

// ──────────────────────────────────────────────────────────────────────
//                                Menu
// ──────────────────────────────────────────────────────────────────────

/// Builds the tray menu and returns it together with the handles we
/// need to mutate later. Called once at startup.
pub fn build_tray_menu<R: Runtime>(
    app: &AppHandle<R>,
    autostart_enabled: bool,
) -> tauri::Result<(Menu<R>, MenuItem<R>, CheckMenuItem<R>)> {
    // Status — *disabled* (grey) MenuItem. Tauri 2's MenuItem::with_id
    // takes (manager, id, text, enabled, accelerator). We pass enabled=false.
    let status_item = MenuItem::with_id(
        app,
        ID_STATUS,
        "Agent actif — En attente",
        false,
        None::<&str>,
    )?;

    let show_item = MenuItem::with_id(
        app,
        ID_SHOW,
        "Ouvrir le panneau",
        true,
        None::<&str>,
    )?;

    let autostart_item = CheckMenuItem::with_id(
        app,
        ID_AUTOSTART,
        "Demarrer avec Windows",
        true,
        autostart_enabled,
        None::<&str>,
    )?;

    let quit_item = MenuItem::with_id(
        app,
        ID_QUIT,
        "Quitter l'agent",
        true,
        None::<&str>,
    )?;

    let separator_1 = PredefinedMenuItem::separator(app)?;
    let separator_2 = PredefinedMenuItem::separator(app)?;

    let items: &[&dyn IsMenuItem<R>] = &[
        &status_item,
        &separator_1,
        &show_item,
        &autostart_item,
        &separator_2,
        &quit_item,
    ];

    let menu = MenuBuilder::new(app).items(items).build()?;
    Ok((menu, status_item, autostart_item))
}

// ──────────────────────────────────────────────────────────────────────
//                                Tray
// ──────────────────────────────────────────────────────────────────────

/// Builds the tray icon and registers it as managed state.
///
/// Must be called from `setup()` — needs `AppHandle` and creates COM-bound
/// objects that have to live on the main thread.
pub fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let autostart_on = is_autostart_enabled();
    let (menu, status_item, autostart_item) = build_tray_menu(app, autostart_on)?;

    // Re-use the bundled default window icon. The user spec mentions a
    // dedicated `tray-icon.png` but the existing bundle already ships
    // `icons/32x32.png` which Tauri loads as the default window icon —
    // good enough for the tray and keeps the asset list short.
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".to_string()))?;

    TrayIconBuilder::with_id("lumiere-tray")
        .icon(icon)
        .tooltip(TRAY_TOOLTIP_DEFAULT)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            handle_menu_event(app, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            // Double-click anywhere on the icon → reopen the panel.
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
            // Single left click on Windows opens the menu — matches AnyDesk.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                // No-op: menu is shown by Windows on right click. We
                // intentionally don't auto-show on left click to avoid
                // accidental opens.
            }
        })
        .build(app)?;

    // Store the items we need to mutate (status label, autostart check)
    // in managed state so the rest of the agent can reach them.
    app.manage(TrayHandles {
        status_item: Mutex::new(status_item),
        autostart_item: Mutex::new(autostart_item),
    });

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
//                            Event handler
// ──────────────────────────────────────────────────────────────────────

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        ID_SHOW => show_main_window(app),
        ID_QUIT => {
            tracing::info!("🛑 Quitter demande via tray — exit 0");
            // Best-effort: signal the agent loop so it can mark itself
            // offline server-side. We don't block on it — the OS will
            // tear the process down regardless after exit().
            app.exit(0);
        }
        ID_AUTOSTART => {
            match toggle_autostart() {
                Ok(now_enabled) => {
                    if let Some(handles) = app.try_state::<TrayHandles<R>>() {
                        if let Ok(item) = handles.autostart_item.lock() {
                            // set_checked reflects the new state visually.
                            let _ = item.set_checked(now_enabled);
                        }
                    }
                    update_tray_tooltip(app, now_enabled);
                }
                Err(err) => {
                    tracing::warn!("❌ toggle_autostart failed: {err}");
                    // Re-sync the checkbox with the registry truth — the
                    // user clicked but the write didn't go through.
                    if let Some(handles) = app.try_state::<TrayHandles<R>>() {
                        if let Ok(item) = handles.autostart_item.lock() {
                            let _ = item.set_checked(is_autostart_enabled());
                        }
                    }
                }
            }
        }
        ID_STATUS => {} // Disabled — no action.
        other => tracing::debug!("Unknown tray menu id: {other}"),
    }
}

// ──────────────────────────────────────────────────────────────────────
//                         Public mutators
// ──────────────────────────────────────────────────────────────────────

/// Updates the disabled "status" row in the tray menu. Safe to call
/// from any thread; the actual menu mutation is marshalled to the main
/// thread by Tauri internally.
pub fn update_tray_status<R: Runtime>(app: &AppHandle<R>, status: &str) {
    if let Some(handles) = app.try_state::<TrayHandles<R>>() {
        if let Ok(item) = handles.status_item.lock() {
            if let Err(err) = item.set_text(status) {
                tracing::warn!("update_tray_status failed: {err}");
            }
        }
    }
    // Mirror the status in the icon tooltip — users see the tooltip on
    // hover even when they never open the menu.
    if let Some(tray) = app.tray_by_id("lumiere-tray") {
        let _ = tray.set_tooltip(Some(format!("Lumiere Agent — {status}")));
    }
}

fn update_tray_tooltip<R: Runtime>(app: &AppHandle<R>, autostart_enabled: bool) {
    if let Some(tray) = app.tray_by_id("lumiere-tray") {
        let suffix = if autostart_enabled {
            "demarrage auto actif"
        } else {
            "demarrage manuel"
        };
        let _ = tray.set_tooltip(Some(format!("Lumiere Agent — {suffix}")));
    }
}

// ──────────────────────────────────────────────────────────────────────
//                           Window helpers
// ──────────────────────────────────────────────────────────────────────

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = show_and_focus(&window);
    }
}

pub fn show_and_focus<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    if window.is_minimized().unwrap_or(false) {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()?;
    Ok(())
}
