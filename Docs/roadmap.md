# AgentsBar Improvement Roadmap

Generated from a comprehensive product and engineering audit, August 2026.

## Quick Wins

| # | Item | Status | Files |
|---|------|--------|-------|
| 1 | **Tile click → open provider dashboard** — provider name in `ProviderTile.svelte` is a clickable button that calls `openUrl(provider.doc_url)` | [DONE] | `src/lib/ProviderTile.svelte` |
| 2 | **Light theme support** — `@media (prefers-color-scheme: light)` block overriding all surface/text/border/state-ramp tokens; `color-scheme: dark light`; tokenized previously hardcoded toggle and scrollbar colors | [DONE] | `src/app.css` |
| 3 | **Test connection button in Settings** — per-provider "Test" button calling `refresh_provider` + `get_snapshots`; shows "OK"/"Failed"/"Testing…" | [DONE] | `src/lib/Settings.svelte` |
| 4 | **Hoist `web_get`/`web_post`/`web_send`/`parse_json` to `util.rs`** — pure refactor, zero behavioral change; removed now-unused imports from `cursor.rs` | [DONE] | `src-tauri/src/providers/util.rs`, `cursor.rs`, `devin.rs`, `opencode.rs`, `t3chat.rs`, `qwen.rs`, `factory.rs` |

## High Impact

| # | Item | Status | Notes |
|---|------|--------|-------|
| 1 | **Windows toast notifications** — fires on `ok → exhausted` transition detected in `scheduler::store()`; uses `tauri-plugin-notification`; Settings toggle "Notify when a provider runs out" (default on) | [DONE] | `Cargo.toml`, `lib.rs`, `capabilities/default.json`, `config.rs`, `scheduler.rs`, `Settings.svelte` |
| 2 | **Set AppUserModelID on Start Menu shortcut** — `tray::ensure_shortcut_aumid()` at startup via `SHGetPropertyStoreFromParsingName` + `IPropertyStore::SetValue`; self-healing for any install method | [DONE] | `tray.rs`, `lib.rs`, `Cargo.toml`, `install.ps1` |

## Medium Impact

| # | Item | Status | Effort | Risks | Dependencies |
|---|------|--------|--------|-------|-------------|
| 1 | **Extended sparkline (7-day trend)** — extend ring buffer from 288 to ~2016 samples; add 24h/7d toggle in tile | [NOT_STARTED] | Medium — extend ring buffer, add toggle | `history.json` grows from ~50KB to ~350KB per provider | None |
| 2 | **Config export/import** — serialize redacted config to JSON for transfer between machines | [NOT_STARTED] | Low — serialize/deserialize, file picker | DPAPI keys won't work cross-machine; import should prompt for re-entry | None |
| 3 | **CLI mode (`agentsbar status --json`)** — enables scripting and IDE integration | [NOT_STARTED] | Medium — Tauri CLI or separate binary | None | None |
| 4 | **Aggregated cost view** — total API spend this month across API-key providers | [NOT_STARTED] | Medium — parse and aggregate non-uniform spend data | None | None |
| 5 | **Proxy configuration** — `reqwest::Proxy` in client builder, one Settings field | [NOT_STARTED] | Low | None | None |

## Nice to Have

| # | Item | Status | Effort |
|---|------|--------|--------|
| 1 | **Provider recommendation engine** — "use Claude, it has the most headroom" | [NOT_STARTED] | Medium — algorithm over snapshot data |
| 2 | **Custom provider support** — provider config schema for self-hosted/niche providers | [NOT_STARTED] | High — need a config schema |
| 3 | **Multi-account display** — backend tracks `account_key`; UI could show which account is active | [NOT_STARTED] | Medium |
| 4 | **Weekly/monthly health log** — "Cursor was down 3 times this week" | [NOT_STARTED] | Medium |
| 5 | **Windows 11 widget** — widget board integration for glanceability without popover | [NOT_STARTED] | High — Windows widget SDK |

## Long-Term Vision

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1: The Monitor | Unified usage visibility across all AI coding tools | [DONE] |
| Phase 2: The Advisor | Provider recommendation, cost optimization, trend analysis, alert rules | [NOT_STARTED] |
| Phase 3: The Orchestrator | Provider switching, quota-aware routing, team dashboards, budget enforcement | [NOT_STARTED] |
| Phase 4: The Platform | Custom provider SDK, plugin system, cross-platform, API for integrations | [NOT_STARTED] |
