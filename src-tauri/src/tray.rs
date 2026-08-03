use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_positioner::{Position, WindowExt};

use crate::config::Config;
use crate::providers::{all_providers, UsageSnapshot};
use crate::state::{lead_window, AppState, DisplayWindow};

pub const TRAY_ID: &str = "agentsbar-tray";

/// The always-on-top desktop widget window. Created on first use, then reused.
pub const WIDGET_LABEL: &str = "widget";

const ICON_SIZE: u32 = 32;
const BAR_TOP: u32 = 27;
const BAR_INSET: u32 = 2;

static STARTUP_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();
static WIDGET_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();
static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
/// When the blur handler last hid the popover. Clicking the tray moves focus away
/// first, so a very recent hide means the click was meant to close the window.
static LAST_HIDE: Mutex<Option<Instant>> = Mutex::new(None);
/// Taskbar theme cache: -1 unknown, 0 dark, 1 light.
static TASKBAR_LIGHT: AtomicI8 = AtomicI8::new(-1);
/// How long after a blur hide a tray click still counts as "was open".
const HIDE_GRACE: Duration = Duration::from_millis(300);

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);

    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Open settings", true, None::<&str>)?;
    let startup = CheckMenuItem::with_id(
        app,
        "startup",
        "Launch at startup",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let updates = MenuItem::with_id(app, "updates", "Check for updates", true, None::<&str>)?;
    let widget = CheckMenuItem::with_id(app, "widget", "Show widget", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&refresh, &settings, &startup, &widget, &updates, &quit],
    )?;
    let _ = STARTUP_ITEM.set(startup);
    let _ = WIDGET_ITEM.set(widget);

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(render_icon(&Glyph::blank()))
        .tooltip("AgentsBar")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "refresh" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    crate::scheduler::refresh_now(&app).await;
                });
            }
            "settings" => {
                show_popover(app);
                let _ = app.emit("open-settings", ());
            }
            "startup" => {
                let checked = STARTUP_ITEM
                    .get()
                    .and_then(|i| i.is_checked().ok())
                    .unwrap_or(false);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    {
                        let state = app.state::<AppState>();
                        let mut cfg = state.config.write().await;
                        cfg.launch_at_startup = checked;
                        if let Err(e) = cfg.save() {
                            log::warn!("config save failed: {e}");
                        }
                    }
                    apply_autostart(&app, checked);
                });
            }
            "updates" => crate::updater::check_now(app),
            "widget" => toggle_widget(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(tray.app_handle());
            }
        })
        .build(app)?;

    // Config is the source of truth for autostart, so reconcile the registry entry
    // and the checkbox with it once the state is available.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let wanted = handle
            .state::<AppState>()
            .config
            .read()
            .await
            .launch_at_startup;
        if wanted != autostart_on {
            apply_autostart(&handle, wanted);
        }
    });

    // Popover semantics: it disappears as soon as it loses focus.
    if let Some(window) = app.get_webview_window("main") {
        round_corners(&window);
        let w = window.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(false) = event {
                if w.is_visible().unwrap_or(false) {
                    let _ = w.hide();
                    note_hide();
                }
            }
        });
    }

    // The widget window is created up front by tauri.conf.json (on the main thread), so
    // showing it is a cheap show/hide and never a window-creation mid-click. It is a
    // transparent window whose rounded corners come from CSS (works on Win10 and Win11),
    // so no DWM rounding here. We just remember drags and restore where it last sat.
    if let Some(window) = app.get_webview_window(WIDGET_LABEL) {
        let app_for_move = app.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Moved(_) = event {
                let should = {
                    let mut last = LAST_WIDGET_SAVE.lock().unwrap();
                    match *last {
                        Some(t) if t.elapsed() < WIDGET_SAVE_EVERY => false,
                        _ => {
                            *last = Some(Instant::now());
                            true
                        }
                    }
                };
                if should {
                    if let Some(w) = app_for_move.get_webview_window(WIDGET_LABEL) {
                        persist_widget_position(&app_for_move, &w);
                    }
                }
            }
        });
        restore_widget_position(app);
    }

    refresh_taskbar_theme();

    Ok(())
}

/// Ask DWM for rounded corners on the popover. The window is undecorated, so without
/// this it draws square corners; Windows 11 rounds them once the preference is set.
/// Windows 10 ignores the attribute and stays square, which is fine. A failure (no DWM,
/// older build) is logged and dropped: square corners are not worth refusing to start.
#[cfg(windows)]
fn round_corners(window: &tauri::WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    unsafe {
        if let Err(e) = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const _,
            std::mem::size_of_val(&preference) as u32,
        ) {
            log::warn!("could not round window corners: {e}");
        }
    }
}

#[cfg(not(windows))]
fn round_corners(_window: &tauri::WebviewWindow) {}

/// Turn autostart on or off and keep the tray checkbox in sync.
pub fn apply_autostart(app: &AppHandle, enabled: bool) {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = result {
        log::warn!("autostart update failed: {e}");
    }
    if let Some(item) = STARTUP_ITEM.get() {
        let _ = item.set_checked(enabled);
    }
}

/// The AppUserModelID, matching the `identifier` in tauri.conf.json. Windows uses this to
/// route toast notifications to the Start Menu shortcut.
const AUMID: &str = "com.agentsbar.app";

/// PROPERTYKEY for System.AppUserModel.ID.
const PKEY_APPUSERMODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F50_4457_A92F_9ECA7CD71F07),
        pid: 5,
    };

/// Ensure the Start Menu shortcut has the AppUserModelID set so toast notifications can
/// find it. Self-healing: works for users who installed via install.ps1, NSIS, or by
/// copying the exe. Silently does nothing if the shortcut or the property store is
/// unavailable.
pub fn ensure_shortcut_aumid() {
    let Some(appdata) = dirs::config_dir() else {
        return;
    };
    let shortcut = appdata.join("Microsoft\\Windows\\Start Menu\\Programs\\AgentsBar.lnk");
    if !shortcut.is_file() {
        return;
    }
    let path = shortcut.to_string_lossy().to_string();

    unsafe {
        use windows::Win32::UI::Shell::PropertiesSystem::{
            IPropertyStore, SHGetPropertyStoreFromParsingName, GETPROPERTYSTOREFLAGS,
        };

        let store: IPropertyStore = match SHGetPropertyStoreFromParsingName(
            &windows::core::HSTRING::from(&path),
            None,
            GETPROPERTYSTOREFLAGS(2), // GPS_READWRITE
        ) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("could not open shortcut property store: {e}");
                return;
            }
        };

        // Already set correctly?
        if let Ok(value) = store.GetValue(&PKEY_APPUSERMODEL_ID) {
            if value.to_string() == AUMID {
                return;
            }
        }

        let prop = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::from(AUMID);
        if store.SetValue(&PKEY_APPUSERMODEL_ID, &prop).is_ok() && store.Commit().is_ok() {
            log::info!("set AppUserModelID on Start Menu shortcut");
        }
    }
}

fn note_hide() {
    if let Ok(mut last) = LAST_HIDE.lock() {
        *last = Some(Instant::now());
    }
}

/// True once per blur hide that is recent enough to belong to the click being handled.
fn consume_recent_hide() -> bool {
    let Ok(mut last) = LAST_HIDE.lock() else {
        return false;
    };
    match *last {
        Some(at) if at.elapsed() < HIDE_GRACE => {
            *last = None;
            true
        }
        _ => false,
    }
}

fn toggle_popover(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // The click already stole the focus, so an open popover is usually hidden by the
    // blur handler before this runs. Without the grace window the click would reopen it.
    if window.is_visible().unwrap_or(false) || consume_recent_hide() {
        let _ = window.hide();
    } else {
        show_popover(app);
    }
}

fn show_popover(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.move_window_constrained(Position::TrayRight);
    let _ = window.show();
    let _ = window.set_focus();

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::scheduler::refresh_if_stale(&app).await;
    });
}

// ---------------------------------------------------------------- desktop widget

/// Throttle for persisting the widget position while it is being dragged: the Moved
/// event fires continuously, and each persist rewrites the whole config, so at most one
/// write per drag interval.
static LAST_WIDGET_SAVE: Mutex<Option<Instant>> = Mutex::new(None);
const WIDGET_SAVE_EVERY: Duration = Duration::from_millis(500);

/// Toggle the always-on-top widget. The window itself is created up front by
/// tauri.conf.json, so this is only ever a show/hide — never a window creation, which on
/// Windows must happen on the main thread and previously deadlocked when invoked from the
/// popover's IPC command thread. All window work is dispatched to the main thread so both
/// call sites (tray menu = already main, popover button = IPC worker) are safe. Unlike
/// the popover, the widget has no blur-hide: it stays put until dismissed.
pub fn toggle_widget(app: &AppHandle) {
    let app_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = app_main.get_webview_window(WIDGET_LABEL) else {
            return;
        };
        if window.is_visible().unwrap_or(false) {
            // A clean save point: wherever it sits now is where it should reopen.
            persist_widget_position(&app_main, &window);
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.set_focus();
            // The widget may have loaded before the first refresh had any numbers, so let
            // it pull fresh data the moment it becomes visible.
            let _ = app_main.emit("widget-shown", ());
        }
        sync_widget_menu(&app_main);
    });
}

/// Restore the widget's saved position (or land it top-right on first use). Reads the
/// config async, then applies the position on the main thread where window work belongs.
fn restore_widget_position(app: &AppHandle) {
    let app_read = app.clone();
    tauri::async_runtime::spawn(async move {
        let pos = {
            let state = app_read.state::<AppState>();
            let cfg = state.config.read().await;
            (cfg.widget_x, cfg.widget_y)
        };
        let app_main = app_read.clone();
        let _ = app_read.run_on_main_thread(move || {
            let Some(window) = app_main.get_webview_window(WIDGET_LABEL) else {
                return;
            };
            match pos {
                (Some(x), Some(y)) => {
                    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
                }
                _ => {
                    let _ = window.move_window_constrained(Position::TopRight);
                }
            }
        });
    });
}

/// Write the widget's current screen position to the config so it reopens there.
fn persist_widget_position(app: &AppHandle, window: &tauri::WebviewWindow) {
    let Ok(pos) = window.outer_position() else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let mut cfg = state.config.write().await;
        cfg.widget_x = Some(pos.x);
        cfg.widget_y = Some(pos.y);
        if let Err(e) = cfg.save() {
            log::warn!("config save failed: {e}");
        }
    });
}

/// Keep the tray checkbox honest whenever the widget's visibility changes, whether from
/// the tray menu itself or from the popover's footer button.
pub fn sync_widget_menu(app: &AppHandle) {
    let Some(item) = WIDGET_ITEM.get() else {
        return;
    };
    let visible = app
        .get_webview_window(WIDGET_LABEL)
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let _ = item.set_checked(visible);
}

// ---------------------------------------------------------------- icon + tooltip

/// Redraw the tray icon and tooltip from the current snapshots.
pub fn update(app: &AppHandle, snapshots: &[UsageSnapshot], config: &Config) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let _ = tray.set_icon(Some(render_icon(&glyph(snapshots, config))));
    let _ = tray.set_tooltip(Some(tooltip(snapshots)));
    // Picks up a theme switch by the next redraw without blocking this one.
    refresh_taskbar_theme();
}

/// What the 32x32 canvas has to say. The icon and the tooltip both come from
/// [`lead_window`], so they cannot name different windows.
struct Glyph {
    text: String,
    /// Percent remaining, for the underline colour only.
    remaining: Option<f64>,
    /// The number is the last good one, not a live one: draw it faded.
    dim: bool,
}

impl Glyph {
    /// No data at all.
    fn blank() -> Self {
        Self {
            text: "--".to_string(),
            remaining: None,
            dim: false,
        }
    }
}

/// The snapshot and window the tray speaks for: the pinned provider when it has a usable
/// number, otherwise the first provider that does.
fn lead<'a>(
    snapshots: &'a [UsageSnapshot],
    config: &Config,
) -> Option<(&'a UsageSnapshot, DisplayWindow)> {
    let usable = |s: &'a UsageSnapshot| {
        lead_window(s)
            .filter(|w| w.used_percent.is_some())
            .map(|w| (s, w))
    };
    config
        .pinned_provider
        .as_deref()
        .and_then(|id| {
            snapshots
                .iter()
                .find(|s| s.provider_id == id)
                .and_then(usable)
        })
        .or_else(|| snapshots.iter().find_map(usable))
}

fn glyph(snapshots: &[UsageSnapshot], config: &Config) -> Glyph {
    let Some((snapshot, window)) = lead(snapshots, config) else {
        return Glyph::blank();
    };
    let remaining = window.used_percent.map(|u| 100.0 - u);
    // A tray reading 0 tells you nothing you can act on; one reading 2h does.
    let text = match remaining {
        Some(left) if left <= 0.0 => window
            .resets_at
            .and_then(|at| countdown(at, chrono::Utc::now()))
            .unwrap_or_else(|| "0".to_string()),
        Some(left) => format!("{}", left.round() as i64),
        None => "--".to_string(),
    };
    Glyph {
        text,
        remaining,
        dim: is_stale(snapshot, config),
    }
}

/// Three refresh intervals without a successful fetch, or an error on the last one.
/// Either way the number on the icon is history and must not look live.
fn is_stale(snapshot: &UsageSnapshot, config: &Config) -> bool {
    if snapshot.error.is_some() {
        return true;
    }
    // Three cadences of the policy actually in use, not three `refresh_minutes`: under
    // adaptive an idle machine batches every 30 minutes and the fixed number is only the
    // interval the user returns to.
    let limit = crate::scheduler::max_cadence_secs(config).saturating_mul(3);
    (chrono::Utc::now() - snapshot.fetched_at).num_seconds() > limit
}

/// Coarse time to a future instant in two or three glyphs: `3d`, `2h`, `34m`.
/// Truncates, so it never promises a shorter wait than the real one.
fn countdown(
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let secs = (at - now).num_seconds();
    if secs <= 0 {
        return None;
    }
    Some(if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}m", (secs / 60).max(1))
    })
}

fn tooltip(snapshots: &[UsageSnapshot]) -> String {
    let names: Vec<(&str, &str)> = all_providers().iter().map(|p| (p.id(), p.name())).collect();
    let mut lines = Vec::new();
    for snap in snapshots {
        let name = names
            .iter()
            .find(|(id, _)| *id == snap.provider_id)
            .map(|(_, n)| *n)
            .unwrap_or(snap.provider_id.as_str());
        // Same helper as the icon, so the two can never name different windows.
        let window = lead_window(snap).filter(|w| w.used_percent.is_some());
        match (window, &snap.error) {
            (Some(w), _) => {
                let left = w.used_percent.map_or(0.0, |u| 100.0 - u);
                let mut line = format!("{name} {}% left", left.round() as i64);
                if let Some(at) = w.resets_at {
                    line.push_str(&format!(
                        ", resets {}",
                        at.with_timezone(&chrono::Local).format("%H:%M")
                    ));
                }
                if let Some(cap) = w.capped_by {
                    line.push_str(&format!(" (capped by {cap})"));
                }
                lines.push(line);
            }
            (None, Some(err)) => lines.push(format!("{name} error: {err}")),
            (None, None) => {}
        }
    }
    if lines.is_empty() {
        "AgentsBar".to_string()
    } else {
        lines.join("\n")
    }
}

fn font() -> Option<&'static FontVec> {
    FONT.get_or_init(|| {
        let fonts = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        for name in ["segoeuib.ttf", "segoeui.ttf"] {
            let path = std::path::Path::new(&fonts).join("Fonts").join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                match FontVec::try_from_vec(bytes) {
                    Ok(f) => return Some(f),
                    Err(e) => log::warn!("failed to parse {}: {e}", path.display()),
                }
            }
        }
        log::warn!("no tray font found, drawing bar only");
        None
    })
    .as_ref()
}

/// Cached answer to "is the taskbar light", so the glyph color costs nothing to pick.
/// Unknown reads as dark, which is the Windows default.
fn light_taskbar() -> bool {
    TASKBAR_LIGHT.load(Ordering::Relaxed) == 1
}

/// Re-read the theme off the icon drawing path. A failed query keeps the last value
/// instead of falling back to white glyphs on a light taskbar.
/// ponytail: spawning `reg` beats pulling in a registry crate for one DWORD.
fn refresh_taskbar_theme() {
    tauri::async_runtime::spawn_blocking(|| {
        if let Some(light) = query_light_taskbar() {
            TASKBAR_LIGHT.store(light as i8, Ordering::Relaxed);
        }
    });
}

fn query_light_taskbar() -> Option<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "/v",
            "SystemUsesLightTheme",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text
        .lines()
        .find(|l| l.contains("SystemUsesLightTheme"))?
        .split_whitespace()
        .next_back()?
        .to_string();
    match value.as_str() {
        "0x1" => Some(true),
        "0x0" => Some(false),
        _ => None,
    }
}

fn bar_color(remaining: Option<f64>) -> [u8; 3] {
    match remaining {
        Some(p) if p > 50.0 => [64, 196, 99],
        Some(p) if p >= 20.0 => [226, 178, 40],
        Some(_) => [226, 74, 62],
        None => [128, 128, 132],
    }
}

/// 32x32 RGBA: the glyph text plus a colored underline bar. A stale glyph is drawn at
/// 55 percent alpha while the bar keeps its last known color, so a dead number reads as
/// dead at a glance without the icon going blank.
fn render_icon(glyph: &Glyph) -> Image<'static> {
    let w = ICON_SIZE as usize;
    let mut buf = vec![0u8; w * w * 4];

    let fg: [u8; 3] = if light_taskbar() {
        [16, 16, 16]
    } else {
        [255, 255, 255]
    };
    draw_text(
        &mut buf,
        &glyph.text,
        fg,
        if glyph.dim { 0.55 } else { 1.0 },
    );

    let [r, g, b] = bar_color(glyph.remaining);
    for y in BAR_TOP..(ICON_SIZE - BAR_INSET) {
        for x in BAR_INSET..(ICON_SIZE - BAR_INSET) {
            let i = (y as usize * w + x as usize) * 4;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = 255;
        }
    }

    Image::new_owned(buf, ICON_SIZE, ICON_SIZE)
}

fn draw_text(buf: &mut [u8], text: &str, color: [u8; 3], alpha: f32) {
    let Some(font) = font() else { return };
    let px = if text.chars().count() >= 3 {
        19.0
    } else {
        25.0
    };
    let scale = PxScale::from(px);
    let scaled = font.as_scaled(scale);

    let ids: Vec<_> = text.chars().map(|c| font.glyph_id(c)).collect();
    let width: f32 = ids.iter().map(|g| scaled.h_advance(*g)).sum();
    let mut x = (ICON_SIZE as f32 - width) / 2.0;
    let baseline = BAR_TOP as f32 - 2.0;
    let w = ICON_SIZE as i32;

    for id in ids {
        let advance = scaled.h_advance(id);
        if let Some(outline) =
            font.outline_glyph(id.with_scale_and_position(scale, point(x, baseline)))
        {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let px = gx as i32 + bounds.min.x as i32;
                let py = gy as i32 + bounds.min.y as i32;
                if px < 0 || py < 0 || px >= w || py >= w {
                    return;
                }
                let i = (py as usize * w as usize + px as usize) * 4;
                let a = (coverage.clamp(0.0, 1.0) * alpha * 255.0) as u8;
                if a > buf[i + 3] {
                    buf[i] = color[0];
                    buf[i + 1] = color[1];
                    buf[i + 2] = color[2];
                    buf[i + 3] = a;
                }
            });
        }
        x += advance;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, used: f64) -> UsageSnapshot {
        let mut s = UsageSnapshot::new(id);
        s.primary = Some(crate::providers::UsageWindow::new(
            "5h",
            Some(used),
            None,
            None,
        ));
        s
    }

    #[test]
    fn icon_and_pinning() {
        // 32x32 RGBA, always fully allocated even without a font.
        let icon = render_icon(&Glyph {
            text: "57".into(),
            remaining: Some(57.0),
            dim: false,
        });
        assert_eq!(icon.rgba().len(), 32 * 32 * 4);
        assert_eq!(render_icon(&Glyph::blank()).rgba().len(), 32 * 32 * 4);

        if font().is_some() {
            // Glyphs must land above the underline bar, not off canvas.
            let text_pixels = icon.rgba()[..BAR_TOP as usize * 32 * 4]
                .chunks(4)
                .filter(|p| p[3] > 0)
                .count();
            assert!(text_pixels > 20, "text did not rasterize: {text_pixels}");
        }

        assert_eq!(bar_color(Some(51.0)), [64, 196, 99]);
        assert_eq!(bar_color(Some(20.0)), [226, 178, 40]);
        assert_eq!(bar_color(Some(19.9)), [226, 74, 62]);

        let snaps = vec![snap("codex", 90.0), snap("claude", 10.0)];
        let mut cfg = Config::default();
        // no pin: first provider with data (codex, 10% left)
        assert_eq!(glyph(&snaps, &cfg).remaining, Some(10.0));
        cfg.pinned_provider = Some("claude".into());
        assert_eq!(glyph(&snaps, &cfg).remaining, Some(90.0));
        // pinned provider without data falls back to the first one
        cfg.pinned_provider = Some("nope".into());
        assert_eq!(glyph(&snaps, &cfg).remaining, Some(10.0));
        assert_eq!(glyph(&[], &cfg).remaining, None);
        assert_eq!(glyph(&[], &cfg).text, "--");
    }

    #[test]
    fn a_blur_hide_makes_the_next_tray_click_close_instead_of_reopen() {
        assert!(!consume_recent_hide(), "nothing hidden yet");
        note_hide();
        assert!(
            consume_recent_hide(),
            "the click that caused the blur closes"
        );
        assert!(!consume_recent_hide(), "and only that one click");
    }

    #[test]
    fn weekly_only_snapshot_still_drives_the_icon() {
        // Codex on Pro reports a weekly window and no primary one.
        let mut s = snap("codex", 51.0);
        s.secondary = s.primary.take().map(|mut w| {
            w.label = "Weekly".into();
            w
        });
        assert_eq!(
            glyph(std::slice::from_ref(&s), &Config::default()).remaining,
            Some(49.0)
        );
        assert!(tooltip(&[s]).contains("49% left"));
    }

    /// Row 1 at the tray boundary: the icon and the tooltip must agree, and both must
    /// report the exhausted weekly rather than the fresh 5h bucket in front of it.
    #[test]
    fn an_exhausted_weekly_paints_the_tray_red_not_green() {
        let mut s = snap("codex", 2.0);
        s.primary = Some(crate::providers::UsageWindow::new(
            "5h",
            Some(2.0),
            None,
            Some(300),
        ));
        s.secondary = Some(crate::providers::UsageWindow::new(
            "Weekly",
            Some(100.0),
            Some(chrono::Utc::now() + chrono::Duration::minutes(150)),
            Some(10080),
        ));
        let cfg = Config::default();
        let g = glyph(std::slice::from_ref(&s), &cfg);
        assert_eq!(g.remaining, Some(0.0), "not the fresh 5h bucket's 98");
        assert_eq!(bar_color(g.remaining), [226, 74, 62]);

        // Row 19: exhausted with a future reset draws the countdown, not a useless 0.
        assert_eq!(g.text, "2h");
        assert!(!g.dim, "a fresh snapshot is not dimmed");

        let tip = tooltip(std::slice::from_ref(&s));
        assert!(tip.contains("0% left"), "{tip}");
        assert!(tip.contains("capped by Weekly"), "{tip}");
    }

    #[test]
    fn a_stale_or_errored_snapshot_dims_the_glyph() {
        // A fixed interval, so the threshold is three of the user's own five minutes.
        let mut cfg = Config {
            refresh_adaptive: false,
            ..Config::default()
        };
        let mut s = snap("codex", 10.0);
        assert!(!is_stale(&s, &cfg));

        s.fetched_at = chrono::Utc::now() - chrono::Duration::minutes(16);
        assert!(is_stale(&s, &cfg), "older than 3 x 5 minute intervals");

        // Row 24: under adaptive an idle machine batches every 30 minutes, so 16 minutes
        // old is current and dimming the glyph for it would be a permanent lie.
        cfg.refresh_adaptive = true;
        assert!(!is_stale(&s, &cfg), "adaptive idles at 30 minute batches");
        s.fetched_at = chrono::Utc::now() - chrono::Duration::minutes(91);
        assert!(is_stale(&s, &cfg));

        let mut errored = snap("codex", 10.0);
        errored.error = Some("http error: 502".into());
        assert!(is_stale(&errored, &cfg));
        assert!(glyph(std::slice::from_ref(&errored), &cfg).dim);
    }

    #[test]
    fn countdowns_are_coarse_and_never_negative() {
        let now = chrono::Utc::now();
        let at = |d: chrono::Duration| countdown(now + d, now);
        assert_eq!(at(chrono::Duration::seconds(-1)), None);
        assert_eq!(at(chrono::Duration::zero()), None);
        // Under a minute still reads as a minute rather than a misleading zero.
        assert_eq!(at(chrono::Duration::seconds(30)).unwrap(), "1m");
        assert_eq!(at(chrono::Duration::minutes(34)).unwrap(), "34m");
        assert_eq!(at(chrono::Duration::hours(2)).unwrap(), "2h");
        assert_eq!(at(chrono::Duration::days(3)).unwrap(), "3d");
    }
}
