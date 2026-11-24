//! Thin Tauri shell over `agent-pin-core`: window placement plus the
//! commands the overlay calls. All status logic lives in the core crate.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_pin_core::approvals::{self, Behavior, Request};
use agent_pin_core::install::HookStatus;
use agent_pin_core::rows::Row;
use agent_pin_core::{build_rows, install, store};
use serde::Serialize;
use tauri::{LogicalSize, Manager, PhysicalPosition, Position, Size, WebviewWindow};

const SCREEN_MARGIN: f64 = 20.0;

/// Session files untouched for this long (3 days) are deleted.
const PRUNE_AFTER_MS: i64 = 259_200_000;
/// How often the app prunes session files after the one at start.
const PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// How often the app refreshes `app.alive`; the hook needs it < 3 s old.
const HEARTBEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// True once the app is exiting. Held while `app.alive` is written, so no
/// heartbeat can recreate the file after exit has removed it.
static HEARTBEAT_STOPPED: Mutex<bool> = Mutex::new(false);

const NO_STATE_DIR: &str = "no state directory: set AGENT_PIN_HOME or the home directory";
const NO_HOME: &str = "Could not resolve home directory";

#[derive(Serialize)]
struct ProviderIntegration {
    id: String,
    /// Kept for older frontends: `status == Current`.
    installed: bool,
    /// `"missing"`, `"stale"` (needs updating) or `"current"`.
    status: HookStatus,
}

#[derive(Serialize)]
struct Integrations {
    providers: Vec<ProviderIntegration>,
    wayland: bool,
}

#[tauri::command]
fn read_rows() -> Vec<Row> {
    let Some(dir) = store::state_dir() else {
        return Vec::new();
    };
    build_rows(&store::list_sessions(&dir), now_ms())
}

#[tauri::command]
fn mark_seen(key: String) -> Result<(), String> {
    let dir = store::state_dir().ok_or(NO_STATE_DIR)?;
    store::mark_seen(&dir, &key, now_ms()).map_err(|error| error.to_string())
}

#[tauri::command]
fn read_integrations() -> Integrations {
    let providers = home_dir()
        .zip(hook_exe().ok())
        .map(|(home, exe)| install::status(&home, &exe))
        .unwrap_or_default()
        .into_iter()
        .map(|(id, status)| ProviderIntegration {
            id,
            installed: status == HookStatus::Current,
            status,
        })
        .collect();
    Integrations {
        providers,
        wayland: wayland_session(),
    }
}

/// Pending approval requests to show, oldest first. Dismissal rules:
/// - `list_pending` deletes expired requests and ones whose hook stopped
///   refreshing the request file (the hook is gone, e.g. killed after the
///   user answered in the terminal);
/// - of the rest, a request whose session row is WaitingApproval or Running
///   is shown; one whose row is Completed, Idle or Error is deleted, best
///   effort; one with no row is hidden but kept.
///
/// See `approvals::pending_for_rows`.
#[tauri::command]
fn read_approvals() -> Vec<Request> {
    let Some(dir) = store::state_dir() else {
        return Vec::new();
    };
    let now = now_ms();
    let pending = approvals::list_pending(&dir, now);
    if pending.is_empty() {
        return pending;
    }
    let rows = build_rows(&store::list_sessions(&dir), now);
    let (keep, ended) = approvals::pending_for_rows(pending, &rows);
    approvals::remove(&dir, &ended);
    keep
}

/// Answers request `id` with Allow or Deny.
#[tauri::command]
fn decide(id: String, allow: bool) -> Result<(), String> {
    let dir = store::state_dir().ok_or(NO_STATE_DIR)?;
    let behavior = if allow { Behavior::Allow } else { Behavior::Deny };
    approvals::decide(&dir, &id, behavior).map_err(|error| error.to_string())
}

/// Whether approving from Agent Pin is on (the default).
#[tauri::command]
fn approvals_enabled() -> bool {
    let Some(dir) = store::state_dir() else {
        return true;
    };
    approvals::enabled(&dir)
}

#[tauri::command]
fn set_approvals_enabled(on: bool) -> Result<(), String> {
    let dir = store::state_dir().ok_or(NO_STATE_DIR)?;
    approvals::set_enabled(&dir, on).map_err(|error| error.to_string())
}

/// Installs hooks. Called only when the user clicks Set up, never on launch.
#[tauri::command]
fn install_integrations() -> Result<Vec<String>, String> {
    install_hooks()
}

/// Resizes the window to the overlay's content. A window sitting in the right
/// half of its monitor keeps its right edge fixed so it grows leftward.
#[tauri::command]
fn fit_window(window: WebviewWindow, width: f64, height: f64) -> Result<(), String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err(format!("invalid window size {width}x{height}"));
    }
    let err = |error: tauri::Error| error.to_string();
    let scale = window.scale_factor().map_err(err)?;
    let position = window.outer_position().map_err(err)?;
    let old_width = window.inner_size().map_err(err)?.width as i32;
    let anchor_right = match window.current_monitor().map_err(err)? {
        Some(monitor) => {
            let center = position.x + old_width / 2;
            center > monitor.position().x + monitor.size().width as i32 / 2
        }
        None => false,
    };

    window
        .set_size(Size::Logical(LogicalSize::new(width, height)))
        .map_err(err)?;

    let new_width = (width * scale).round() as i32;
    if anchor_right && new_width != old_width {
        let x = position.x + old_width - new_width;
        window
            .set_position(Position::Physical(PhysicalPosition::new(x, position.y)))
            .map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
fn start_window_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|error| error.to_string())
}

/// Installs agent-pin hooks for every provider whose config directory exists,
/// returning the provider ids installed. Shared by the Set up button and the
/// `install-hooks` CLI subcommand.
pub fn install_hooks() -> Result<Vec<String>, String> {
    let home = home_dir().ok_or(NO_HOME)?;
    let exe = hook_exe()?;
    validate_install_exe_path(&exe)?;
    install::install(&home, &exe, now_ms() / 1000)
}

/// Current time in Unix milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// The user's home: `USERPROFILE` on Windows, `HOME` elsewhere.
fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// The exe the installed hook commands run. On Linux inside an AppImage this
/// is the AppImage file itself, since `current_exe` is a temporary mount that
/// vanishes when the app quits; see [`install::choose_exe`].
fn hook_exe() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    if !cfg!(target_os = "linux") {
        return Ok(current);
    }
    Ok(install::choose_exe(
        current,
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
    ))
}

/// True on Linux under a Wayland session, where always-on-top may be ignored.
fn wayland_session() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
}

#[cfg(target_os = "macos")]
fn validate_install_exe_path(path: &Path) -> Result<(), String> {
    let text = path.display().to_string();
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        return Err("Move Agent Pin to Applications, reopen it, then run setup again.".to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn validate_install_exe_path(_path: &Path) -> Result<(), String> {
    Ok(())
}

pub fn run() {
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            read_rows,
            mark_seen,
            read_integrations,
            install_integrations,
            read_approvals,
            decide,
            approvals_enabled,
            set_approvals_enabled,
            fit_window,
            start_window_drag
        ])
        .setup(|app| {
            spawn_pruner();
            spawn_heartbeat();
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(true);
                let _ = position_top_right(&window);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running Agent Pin");
    app.run(|_app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            stop_heartbeat();
        }
    });
}

/// Refreshes `app.alive` every second so hooks know the app can answer
/// approval requests. Errors are ignored and a panic is caught, so the
/// heartbeat never takes the app down.
fn spawn_heartbeat() {
    std::thread::spawn(|| loop {
        let _ = std::panic::catch_unwind(|| {
            let stopped = HEARTBEAT_STOPPED.lock().unwrap_or_else(|e| e.into_inner());
            if !*stopped {
                if let Some(dir) = store::state_dir() {
                    let _ = approvals::write_alive(&dir, now_ms());
                }
            }
        });
        std::thread::sleep(HEARTBEAT_EVERY);
    });
}

/// Stops the heartbeat and removes `app.alive` if this process wrote it,
/// best effort, so waiting hooks give up at once instead of after the file
/// goes stale. Another instance's file is left alone.
fn stop_heartbeat() {
    let mut stopped = HEARTBEAT_STOPPED.lock().unwrap_or_else(|e| e.into_inner());
    *stopped = true;
    if let Some(dir) = store::state_dir() {
        let _ = approvals::remove_alive(&dir);
    }
}

/// Prunes stale session files now and then every hour, on a background
/// thread so a busy session lock never stalls the UI.
fn spawn_pruner() {
    std::thread::spawn(|| loop {
        if let Some(dir) = store::state_dir() {
            if let Err(error) = store::prune(&dir, now_ms(), PRUNE_AFTER_MS) {
                let _ = writeln!(std::io::stderr(), "agent-pin: prune {}: {error}", dir.display());
            }
        }
        std::thread::sleep(PRUNE_EVERY);
    });
}

/// Places the window in the top-right corner of its monitor, `SCREEN_MARGIN`
/// logical pixels from the edges.
fn position_top_right(window: &WebviewWindow) -> Result<(), String> {
    let err = |error: tauri::Error| error.to_string();
    let monitor = window
        .current_monitor()
        .map_err(err)?
        .ok_or_else(|| "No monitor available".to_string())?;
    let margin = (SCREEN_MARGIN * monitor.scale_factor()).round() as i32;
    let width = window.outer_size().map_err(err)?.width as i32;
    let x = monitor.position().x + monitor.size().width as i32 - width - margin;
    let y = monitor.position().y + margin;
    window
        .set_position(Position::Physical(PhysicalPosition::new(x, y)))
        .map_err(err)
}
