# AgentsBar

> Every AI coding limit in your Windows tray. Codex, Claude, Gemini, Copilot, Cursor and 19 more, no logins.

A tiny Windows tray app that shows how much of your AI coding quota is left and when it
resets. Windows port of [CodexBar](https://github.com/steipete/CodexBar).

6 MB, ~30 MB RAM, 0% CPU idle. No account, no telemetry.

## Install

```powershell
voli install agentsbar
```

Or grab `AgentsBar_x64-setup.exe` from [Releases](https://github.com/Topurrra/AgentsBar/releases),
or run:

```powershell
iwr -useb https://github.com/Topurrra/AgentsBar/releases/latest/download/install.ps1 | iex
```

All per-user: no admin, no PATH changes. Updates are checked on start and installed only
if you say yes.

## What you get

- Tray icon with percent left for your pinned provider, or a countdown when it is spent
- Popover with a tile per provider: usage bars, reset countdowns, credits, plan, pace
  ("runs out in 1d 21h"), and a 24h / 7d sparkline
- Tiles sorted by whatever is closest to running out
- Right click for refresh, settings, launch at startup, updates

## Command line

The same exe doubles as a CLI, so you can script it or pipe it into other tools. It reads
the numbers the tray already collected (no extra requests to the providers).

```powershell
agentsbar status            # every provider's current usage, one per line
agentsbar status --json     # the same data as JSON
agentsbar --version
agentsbar --help
```

`status` shows a glyph per provider (`✓` plenty, `!` getting low, `×` exhausted or error,
`◆` a dollar figure, `-` no data yet), the headline number, when it resets, and a total of
USD spend and balance across the providers that report dollars. Run it from a terminal;
colours switch off automatically when you redirect (`agentsbar status --json | jq`).

## Providers

Sign in with the CLI or browser you already use. Most need no configuration.

| Auth | Providers |
|------|-----------|
| CLI session, automatic | Codex, Claude, Gemini, GitHub Copilot |
| API key, paste in Settings | OpenAI, z.ai, MiniMax, Kimi, OpenRouter, DeepSeek, ElevenLabs, Deepgram, xAI, Warp |
| Browser session, automatic | Cursor, Factory, T3 Chat, OpenCode, Manus, Augment, Amp, Qwen Cloud |
| Token pasted in Settings | Devin, Windsurf |

## Privacy

- No passwords, ever. It reuses sessions you already have.
- Reads only known credential and cookie paths, and only for providers you enabled.
- Keys stay on your machine, encrypted with Windows DPAPI, and go only to the provider
  they belong to.
- Chrome and Edge app-bound cookies (v20) are skipped, not bypassed. Use another browser
  or paste a cookie header if a provider needs one.

Settings and history live in `%APPDATA%\AgentsBar`.

## Build

Rust, Node 18+, Windows 10/11 x64.

```powershell
npm install
npx tauri build
```

The `no private key` line at the end is expected; it only skips the signed update
manifest.

## Credits

[CodexBar](https://github.com/steipete/CodexBar) by Peter Steinberger, whose provider
research this port is built on. Provider names and logos belong to their owners;
AgentsBar is not affiliated with any of them.

MIT, see [LICENSE](LICENSE).
