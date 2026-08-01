# AgentsBar

> Every AI coding limit in your Windows tray. Codex, Claude, Gemini, Copilot, Cursor and 18 more, no logins.

AgentsBar is a tiny Windows tray app that keeps AI coding provider usage limits visible:
percent remaining, reset countdowns, credits, and plan info. It is a native Windows port
of the excellent macOS app [CodexBar](https://github.com/steipete/CodexBar) by
Peter Steinberger (MIT).

Built with Rust, Tauri 2, and Svelte 5. Fast and lightweight by design:

- 6.25 MB executable, no bundled browser (uses the Windows WebView2 runtime)
- ~30 MB memory while running, 0% CPU when idle
- No accounts, no telemetry, no passwords stored. AgentsBar reuses the sessions your
  CLIs and browser already have, or API keys you provide.

## Features

- Live tray icon showing percent remaining for your pinned provider, with a colored
  status bar (green, yellow, red) and light/dark taskbar aware text. When the leading
  window is exhausted the icon shows the countdown to its reset instead of a bare zero
- Tooltip summary of every enabled provider with reset times
- Click the tray icon for a compact popover: per-provider tiles with session, weekly,
  and model-specific usage bars, reset countdowns, credits, plan, and account. Tiles are
  sorted by urgency, so whatever is closest to running out is at the top
- Real provider logos on every tile, and a sparkline per provider showing how the last
  few hours of usage moved
- A pace line under every bar: whether you are ahead or behind for the window, and
  whether what is left lasts to the reset
- Right-click menu: refresh now, settings, launch at startup, check for updates, quit
- Settings for refresh cadence, pinned provider, enabling providers, API keys, and the
  browser each cookie based provider reads from
- Automatic OAuth token refresh for Codex, Claude, and Gemini, merged back into the
  CLI credential files exactly like the CLIs do

## Supported providers

23 providers. Auth is detected automatically for the CLI providers; API key providers
activate when you save a key in Settings; cookie providers reuse the session already in
your browser (see [Privacy and trust](#privacy-and-trust) below).

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
| Warp | API key |
| Cursor | Browser session cookie (`cursor.com`) |
| Factory | Browser session cookie (`factory.ai`, `workos.com`) |
| T3 Chat | Browser session cookie (`t3.chat`) |
| OpenCode | Browser session cookie (`opencode.ai`) |
| Manus | Browser session cookie (`manus.im`) |
| Augment | Browser session cookie (`augmentcode.com`) |
| Amp | Browser session cookie (`ampcode.com`) |
| Devin | Session token pasted in Settings |
| Windsurf | Session bundle pasted in Settings |

The provider framework is one file plus one registry line per provider, so more of
CodexBar's 66 providers can land incrementally.

## Privacy and trust

AgentsBar reads browser cookie databases. That deserves a plain explanation rather than a
promise, so here is exactly what it does and does not do.

**No passwords, ever.** AgentsBar never asks for, prompts for, or stores a password for
any provider or for your browser. It reuses sessions that already exist on your machine.

**It does not scan your disk.** It reads a fixed list of known paths, and only the ones
belonging to features you have enabled:

- `%USERPROFILE%\.codex\auth.json` (or `%CODEX_HOME%`), for the Codex CLI session
- `%USERPROFILE%\.claude\.credentials.json` (or `%CLAUDE_CONFIG_DIR%`), for Claude Code
- `%USERPROFILE%\.gemini\oauth_creds.json`, for the Gemini CLI
- `%LOCALAPPDATA%\github-copilot\apps.json` and `hosts.json`, for GitHub Copilot
- `%LOCALAPPDATA%\Google\Chrome\User Data\<profile>\Network\Cookies` and
  `...\Local State`, and the same two files under
  `%LOCALAPPDATA%\Microsoft\Edge\User Data` and
  `%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data`
- `%APPDATA%\Mozilla\Firefox\Profiles\<profile>\cookies.sqlite`
- `%APPDATA%\AgentsBar\config.json` and `history.json`, its own settings and sparkline data

Cookie databases are read only for the specific domains of the providers you enabled, and
only the cookies those providers need. Nothing else in the database is read, kept, or
sent anywhere.

**Cookie reads work on a copy.** A running browser holds its database open, so AgentsBar
copies the file into `%LOCALAPPDATA%\AgentsBar\tmp`, reads it, and deletes the copy
immediately. Not `%TEMP%`: that is only per-user by default, and on a machine where it has
been redirected to a shared folder every local account can read it. Anything a killed run
left behind is swept at the next start, in both locations.

**Secrets never leave your machine except to the provider they authenticate.** API keys,
tokens, and imported cookies go to the provider's own HTTPS endpoint and nowhere else.
There is no AgentsBar server, no analytics and no crash reporting. AgentsBar does check
GitHub for a new release when it starts and when you ask it to; that request carries
nothing about you, and no update is ever installed without asking. The shared HTTP client
refuses to follow a redirect that leaves the original HTTPS origin, so a credentialed
request cannot be steered somewhere else.

**We skip app-bound encrypted cookies, we do not bypass them.** Chrome and Edge now
encrypt newer cookies with app-bound encryption (v20). Reading those requires COM
elevation, browser process impersonation, or raw NTFS sector reads, which are attacks on
a security control and the exact behavior an infostealer performs. AgentsBar refuses to do
it: v20 rows are skipped, the affected browser is reported as partially supported in
Settings, and you are pointed at another browser or at pasting a cookie header yourself.
A cookie library was rejected as a dependency for shipping precisely that bypass by
default.

**Logs cannot leak credentials.** Every log line passes through a redactor at the sink:
`Cookie` and `Authorization` header values, bearer tokens, `sk-...` and GitHub `ghp_...`
style tokens, and email addresses are replaced before the line is written.

**Where your own secrets live.** API keys and any manually pasted cookie headers are
stored in `%APPDATA%\AgentsBar\config.json` wrapped with Windows DPAPI
(`CryptProtectData`, current user scope) behind a `dpapi:` prefix. Be clear about what
that buys you: it does **not** stop code running as you. Anything running under your
account can call `CryptUnprotectData` and read those values back, exactly the way AgentsBar
does. It is not a keystore and there is no master password. What it does stop is the file
leaking as readable text: an `%APPDATA%` folder synced to OneDrive, a config pasted into a
bug report, a backup or disk image opened on another machine, and file-scraping stealers
that grab known config paths without executing anything. Plaintext values still load, so
the file stays hand editable and a config from an older build is never locked out; it is
re-wrapped on the next save. AgentsBar also redacts those fields whenever it hands the
config to its own UI, and the diagnostics report in Settings contains none of them.

## Install

Windows 10 or 11, x64. Every option is a per-user install: no administrator rights, no
PATH changes, nothing written outside your user profile.

### 1. PowerShell one-liner (recommended)

```powershell
iwr -useb https://volibear.dev/agentsbar | iex
```

If that host is unreachable, the same script is always available from the release itself:

```powershell
iwr -useb https://github.com/Topurrra/AgentsBar/releases/latest/download/install.ps1 | iex
```

It downloads `AgentsBar-x64.zip` from the latest GitHub release, verifies its published
SHA256, extracts it to `%LOCALAPPDATA%\Programs\AgentsBar`, and creates a Start Menu
shortcut. To uninstall: quit AgentsBar, delete that folder and the Start Menu shortcut,
remove the autostart entry with
`Remove-ItemProperty 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run' -Name AgentsBar -ErrorAction SilentlyContinue`
(the app writes it when you enable "Launch at startup"; left behind it points at a deleted
exe at every logon), and delete `%APPDATA%\AgentsBar` if you also want your settings gone.
The script prints all of this when it finishes.

### 2. voli

If you use [voli](https://volibear.dev):

```powershell
voli install agentsbar
```

This installs the same portable zip as the one-liner.

### 3. NSIS installer

Download `AgentsBar_<version>_x64-setup.exe` (for example `AgentsBar_0.2.0_x64-setup.exe`)
from [Releases](https://github.com/Topurrra/AgentsBar/releases/latest) and run it.

The installer is not code signed, so Windows SmartScreen shows "Windows protected your
PC" the first time you run it. Choose **More info**, then **Run anyway**. Every release
publishes a SHA256 you can check first with
`Get-FileHash .\AgentsBar_0.2.0_x64-setup.exe`.

### 4. Build from source

Requirements: Rust (stable), Node.js 18+, Windows 10/11.

```bash
npm install
npx tauri build
```

The installer lands in `src-tauri/target/release/bundle/nsis/`, the portable exe in
`src-tauri/target/release/agentsbar.exe`. For development use `npx tauri dev`.

The last line of that build reads `A public key has been found, but no private key`. That
is expected and harmless when you build from source: the installer and the exe are already
finished at that point, and only the signed updater manifest is skipped, because signing it
needs the AgentsBar release private key. Your build works; it simply cannot publish an
update that existing installs would accept.

### Updates

If you used the NSIS installer, AgentsBar checks for updates when it starts and from the
tray menu. It never installs one without asking, and it will only install an update signed
with the AgentsBar release key.

The other install routes update through whatever put the app there, so the in-app updater
stays out of their way: it compares its own location against the directory the NSIS
installer recorded and does nothing when they differ. The startup check is silent about
it; the tray's **Check for updates** says so and tells you the right command. Otherwise
accepting an update would install a second copy in a different folder, with its own Start
Menu and Add/Remove Programs entries, while your shortcut kept pointing at the old one.

- One-liner: re-run `iwr -useb https://volibear.dev/agentsbar | iex`
- voli: `voli update agentsbar`
- Built from source: `git pull` and `npx tauri build`

## Configuration

Settings live in `%APPDATA%\AgentsBar\config.json` (refresh cadence, enabled
providers, API keys, cookie source per provider, pinned provider, launch at startup).
Everything is editable from the Settings view in the popover.

**Refresh cadence.** Adaptive is the default on a fresh install: AgentsBar refreshes every
2 minutes while you are actively opening the popover and backs off to 30 minutes when you
are not, or under Windows battery saver. Pick a fixed interval instead if you would rather
it be predictable. An existing install keeps the interval it already had.

**API keys from the environment.** If you already export a provider's key, AgentsBar reads
it and you do not have to paste it into Settings. A key saved in Settings always wins, so
a stale export can never silently shadow it. The variables are `OPENAI_API_KEY`,
`OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`, `ELEVENLABS_API_KEY`, `DEEPGRAM_API_KEY`,
`Z_AI_API_KEY`, `MINIMAX_API_KEY`, `KIMI_CODE_API_KEY`, `XAI_MANAGEMENT_API_KEY` and
`WARP_API_KEY`.

**Support.** Settings has a **Copy report** button that puts a diagnostics summary on the
clipboard for a bug report. It lists versions, which providers are configured and how each
one last fared, and it deliberately contains no keys, cookies, tokens or email addresses.
**Clear cached cookies** re-reads your browser immediately, which is the fix after signing
back in to a provider.

## Credits

- [CodexBar](https://github.com/steipete/CodexBar) by Peter Steinberger, the original
  macOS app this project ports, including its provider research and API mappings
- Built with [Tauri](https://tauri.app), [Svelte](https://svelte.dev), and Rust

## Trademarks

Provider names and logos are used only to identify the services whose usage AgentsBar
displays. They remain the property of their respective owners. AgentsBar is an independent
project and is not affiliated with, endorsed by, or sponsored by any of them.

## License

MIT, see [LICENSE](LICENSE).
