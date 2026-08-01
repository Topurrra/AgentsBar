//! In-app updates (SPEC4 row 29).
//!
//! A shipped build with no update channel is unreachable forever, so this has to exist
//! in the first public build rather than the second. The feed is a signed `latest.json`
//! published on GitHub Releases; the public key is pinned in `tauri.conf.json` and the
//! private half never enters the repo (see `Docs/RELEASING.md`).
//!
//! The one rule the code enforces: an update is NEVER installed silently. Both entry
//! points ask first, and answering no is a full stop until the next check.

use std::iter::once;

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;
use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MESSAGEBOX_STYLE,
};

/// Startup check. Stays quiet unless there is actually something to install, because a
/// tray app that opens a box saying "nothing happened" every launch gets uninstalled.
pub fn check_on_startup(app: &AppHandle) {
    spawn(app.clone(), false);
}

/// The tray menu item. Always answers, since the user just asked a question.
pub fn check_now(app: &AppHandle) {
    spawn(app.clone(), true);
}

fn spawn(app: AppHandle, announce: bool) {
    tauri::async_runtime::spawn(async move { check(app, announce).await });
}

async fn check(app: AppHandle, announce: bool) {
    let found = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => {
            log::info!("update check: already current");
            if announce {
                tell("AgentBar is up to date.").await;
            }
            return;
        }
        Err(e) => {
            log::warn!("update check failed: {e}");
            if announce {
                tell(&format!("Could not check for updates.\n\n{e}")).await;
            }
            return;
        }
    };

    let prompt = format!(
        "AgentBar {} is available. You have {}.\n\nDownload and install it now? AgentBar will restart.",
        update.version, update.current_version
    );
    if !ask(&prompt).await {
        log::info!("update {} declined", update.version);
        return;
    }

    log::info!("installing update {}", update.version);
    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => app.restart(),
        Err(e) => {
            log::warn!("update install failed: {e}");
            tell(&format!(
                "The update could not be installed.\n\n{e}\n\nYou can download it manually from the AgentBar releases page."
            ))
            .await;
        }
    }
}

/// ponytail: `MessageBoxW` through the `windows` crate we already depend on, instead of
/// pulling `tauri-plugin-dialog` (and `rfd`) in for two boxes. It blocks its thread, so
/// it runs on a blocking one and never stalls the async runtime or the UI.
async fn message_box(text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    let text = wide(text);
    let caption = wide("AgentBar");
    tauri::async_runtime::spawn_blocking(move || unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            style | MB_SETFOREGROUND | MB_TOPMOST,
        )
        .0
    })
    .await
    .unwrap_or_default()
}

async fn ask(text: &str) -> bool {
    message_box(text, MB_YESNO | MB_ICONQUESTION).await == IDYES.0
}

async fn tell(text: &str) {
    message_box(text, MB_OK | MB_ICONINFORMATION).await;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::wide;

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("hi"), vec![b'h' as u16, b'i' as u16, 0]);
        assert_eq!(wide(""), vec![0]);
    }
}
