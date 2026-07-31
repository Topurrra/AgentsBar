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
