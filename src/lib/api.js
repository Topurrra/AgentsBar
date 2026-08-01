import { invoke } from "@tauri-apps/api/core";

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

// Wave 2 commands. Thin wrappers so component code never repeats a command name.
export const listBrowsers = () => call("list_browsers");
export const getHistory = () => call("get_history");

export const setCookieSource = (id, source, browser) =>
  call("set_cookie_source", { id, source, browser: browser || null });

// header is a secret: pass it straight through, never keep it in component state.
export const setCookieHeader = (id, header) => call("set_cookie_header", { id, header });
