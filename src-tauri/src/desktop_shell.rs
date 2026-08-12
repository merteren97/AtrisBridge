use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, Runtime, Window, WindowEvent,
};

const TRAY_ID: &str = "atrisbridge-main";
const SHOW_ID: &str = "tray-show";
const QUIT_ID: &str = "tray-quit";
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(false);

pub fn setup(app: &mut App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, SHOW_ID, "Open AtrisBridge", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit AtrisBridge", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("AtrisBridge — project continuity")
        .on_menu_event(|app, event| match event.id().as_ref() {
            SHOW_ID => show_main_window(app),
            QUIT_ID => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }

    let tray = tray.build(app)?;
    // Quit-on-close remains the safe default. The persisted frontend preference
    // is synchronized immediately after hydration and enables the tray only when requested.
    tray.set_visible(false)?;
    Ok(())
}

#[tauri::command]
pub fn set_close_to_tray(app: AppHandle, enabled: bool) -> Result<(), String> {
    CLOSE_TO_TRAY.store(enabled, Ordering::SeqCst);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_visible(enabled)
            .map_err(|error| format!("Could not update AtrisBridge tray visibility: {error}"))?;
    }
    Ok(())
}

pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }

    if let WindowEvent::CloseRequested { api, .. } = event {
        if CLOSE_TO_TRAY.load(Ordering::SeqCst) {
            api.prevent_close();
            let _ = window.hide();
        } else {
            window.app_handle().exit(0);
        }
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
