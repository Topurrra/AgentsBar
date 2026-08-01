//! In-app updates (SPEC4 row 29).
//!
//! A shipped build with no update channel is unreachable forever, so this has to exist
//! in the first public build rather than the second. The feed is a signed `latest.json`
//! published on GitHub Releases; the public key is pinned in `tauri.conf.json` and the
//! private half never enters the repo (see `Docs/RELEASING.md`).
//!
//! The one rule the code enforces: an update is NEVER installed silently. Both entry
//! points ask first, and answering no is a full stop until the next check.
//!
//! The second rule: this path only runs for a copy the NSIS installer wrote. The plugin
//! ends by handing the downloaded setup exe to `ShellExecuteW` and exiting, and that
//! installer always writes to its own install root. Run it from a portable copy (the
//! `install.ps1` one-liner, `voli install agentsbar`, or a `target\release` build) and it
//! would create a SECOND install somewhere else, with its own Start Menu and Add/Remove
//! entries, while the shortcut and the autostart value still point at the stale copy.

use std::iter::once;
use std::path::{Path, PathBuf};

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
    if !is_installed_copy() {
        log::info!("update check skipped: this copy was not installed by the NSIS installer");
        return;
    }
    spawn(app.clone(), false);
}

/// The tray menu item. Always answers, since the user just asked a question.
pub fn check_now(app: &AppHandle) {
    if !is_installed_copy() {
        tauri::async_runtime::spawn(async {
            tell(
                "This copy of AgentsBar was not installed by the AgentsBar installer, so it \
                 updates itself through whatever put it here.\n\n\
                 Portable or one-liner install: re-run\n\
                 iwr -useb https://volibear.dev/agentsbar | iex\n\n\
                 voli install: run\n\
                 voli update agentsbar",
            )
            .await;
        });
        return;
    }
    spawn(app.clone(), true);
}

/// Where the NSIS installer records the directory it wrote to. Reading it rather than
/// assuming `%LOCALAPPDATA%\AgentsBar` keeps this correct when the user picked a different
/// directory on the installer's Destination page.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\AgentsBar";

fn nsis_install_dir() -> Option<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(UNINSTALL_KEY)
        .ok()?;
    let raw: String = key.get_value("InstallLocation").ok()?;
    // NSIS writes the value quoted.
    Some(PathBuf::from(raw.trim_matches('"')))
}

fn is_installed_copy() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    nsis_install_dir().is_some_and(|dir| exe.parent().is_some_and(|p| same_dir(p, &dir)))
}

/// Windows paths compare case-insensitively and can differ by trailing separator or short
/// name, so canonicalize rather than compare the strings. Both sides must resolve.
fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
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
                tell("AgentsBar is up to date.").await;
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
        "AgentsBar {} is available. You have {}.\n\nDownload and install it now? AgentsBar will restart.",
        update.version, update.current_version
    );
    if !ask(&prompt).await {
        log::info!("update {} declined", update.version);
        return;
    }

    log::info!("installing update {}", update.version);
    match update.download_and_install(|_, _| {}, || {}).await {
        // Unreachable on Windows: the plugin's install path ends in ShellExecuteW on the
        // downloaded setup exe followed by std::process::exit(0), so this never returns Ok.
        // The restart the prompt promises is done by the installer's /UPDATE flag, which
        // starts the newly written exe. Kept so the arm is not silently non-exhaustive.
        Ok(()) => app.restart(),
        Err(e) => {
            log::warn!("update install failed: {e}");
            tell(&format!(
                "The update could not be installed.\n\n{e}\n\nYou can download it manually from the AgentsBar releases page."
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
    let caption = wide("AgentsBar");
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
    use super::{same_dir, wide};
    use std::path::{Path, PathBuf};

    #[test]
    fn wide_strings_are_nul_terminated() {
        assert_eq!(wide("hi"), vec![b'h' as u16, b'i' as u16, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    /// The gate that decides whether the in-app updater may run at all. Getting this wrong
    /// in the permissive direction means a portable install grows a second copy of itself.
    #[test]
    fn the_install_dir_comparison_ignores_case_and_trailing_separators() {
        let root: PathBuf =
            std::env::temp_dir().join(format!("agentsbar-dir-{}", std::process::id()));
        let here = root.join("Programs\\AgentsBar");
        let sibling = root.join("Programs\\AgentsBar-old");
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(same_dir(&here, &here));
        let shouty = here.to_string_lossy().to_uppercase();
        assert!(same_dir(&here, Path::new(&shouty)));
        assert!(same_dir(&here, Path::new(&format!("{}\\", here.display()))));
        assert!(!same_dir(&here, &sibling));
        // A recorded install directory that no longer exists is not a match.
        assert!(!same_dir(&here, &root.join("gone")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
