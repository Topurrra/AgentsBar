# AgentBar

> Every AI coding limit, in your Windows system tray.

AgentBar is a tiny Windows tray app that keeps AI coding provider usage limits visible:
percent remaining, reset countdowns, credits, and plan info. It is a native Windows port
of the excellent macOS app [CodexBar](https://github.com/steipete/CodexBar) by
Peter Steinberger (MIT).

Built with Rust, Tauri 2, and Svelte 5. Fast and lightweight by design:

- 4.6 MB executable, no bundled browser (uses the Windows WebView2 runtime)
- ~27 MB memory while running, 0% CPU when idle
- No accounts, no telemetry, no passwords stored. AgentBar reuses the sessions your
  CLIs already have (OAuth credential files) or API keys you provide.

## Features

- Live tray icon showing percent remaining for your pinned provider, with a colored
  status bar (green, yellow, red) and light/dark taskbar aware text
- Tooltip summary of every enabled provider with reset times
- Click the tray icon for a compact popover: per-provider tiles with session, weekly,
  and model-specific usage bars, reset countdowns, credits, plan, and account
- Right-click menu: refresh now, settings, launch at startup, quit
- Settings for refresh interval, pinned provider, enabling providers, and API keys
- Automatic OAuth token refresh for Codex, Claude, and Gemini, persisted back to the
  CLI credential files exactly like the CLIs do

## Supported providers

Auth is detected automatically for CLI providers; API key providers activate when you
save a key in Settings.

| Provider | Source |
|----------|--------|
| Codex | Codex CLI OAuth (`~/.codex/auth.json`, `CODEX_HOME` respected) |
| Claude | Claude Code OAuth (`~/.claude/.credentials.json`, `CLAUDE_CONFIG_DIR` respected) |
| Gemini | Gemini CLI OAuth (`~/.gemini/oauth_creds.json`) |
| GitHub Copilot | Copilot login (`%LOCALAPPDATA%\github-copilot`) |
| OpenAI | Admin API key |
| z.ai | API token (region aware) |
| MiniMax | API key (.io and .cn hosts) |
| Kimi | Auth token |
| OpenRouter | API key |
| DeepSeek | API key |
| ElevenLabs | API key |
| Deepgram | API key |
| xAI | API key |

The provider framework is one file plus one registry line per provider, so more of
CodexBar's 66 providers can land incrementally (browser cookie based providers such as
Cursor are planned for a later wave).

## Install

Download `AgentBar_x64-setup.exe` from Releases and run it, or build from source.

### Build from source

Requirements: Rust (stable), Node.js 18+, Windows 10/11.

```bash
npm install
npx tauri build
```

The installer lands in `src-tauri/target/release/bundle/nsis/`, the portable exe in
`src-tauri/target/release/agentbar.exe`. For development use `npx tauri dev`.

## Configuration

Settings live in `%APPDATA%\AgentBar\config.json` (refresh interval, enabled
providers, API keys, pinned provider, launch at startup). Everything is editable from
the Settings view in the popover; API keys never leave the backend except to the
provider they belong to.

## Credits

- [CodexBar](https://github.com/steipete/CodexBar) by Peter Steinberger, the original
  macOS app this project ports, including its provider research and API mappings
- Built with [Tauri](https://tauri.app), [Svelte](https://svelte.dev), and Rust

## License

MIT, see [LICENSE](LICENSE).
