import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Backend commands may be stubs or fail while the app is starting.
// Every call resolves: null on failure, so the UI never breaks.
export async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    console.error(cmd, e);
    return null;
  }
}

// ponytail: direct plugin invoke instead of adding @tauri-apps/plugin-opener.
export function openUrl(url) {
  if (url) call("plugin:opener|open_url", { url });
}

// Escape closes the popover. Hiding from here rather than through a command on purpose:
// tray.rs only arms its reopen grace window when the blur handler is the one hiding, so
// hiding first leaves the next tray click free to reopen.
export const hidePopover = () => getCurrentWindow().hide().catch(() => {});

// Wave 2 commands. Thin wrappers so component code never repeats a command name.
export const listBrowsers = () => call("list_browsers");
export const getHistory = () => call("get_history");
export const getHealth = () => call("get_health");

export const setCookieSource = (id, source, browser) =>
  call("set_cookie_source", { id, source, browser: browser || null });

// header is a secret: pass it straight through, never keep it in component state.
export const setCookieHeader = (id, header) => call("set_cookie_header", { id, header });

// Wave 4 support commands. `export_diagnostics` returns a report that is already free of
// keys, cookies and emails on the Rust side, so the UI never has to redact anything.
export const exportDiagnostics = () => call("export_diagnostics");
// Returns nothing on success, so `call`'s null-on-failure convention cannot be read
// here: this one reports the boolean instead.
export const clearCookieCache = async () => {
  try {
    await invoke("clear_cookie_cache");
    return true;
  } catch (e) {
    console.error("clear_cookie_cache", e);
    return false;
  }
};
