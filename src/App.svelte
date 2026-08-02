<script>
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { call, getHistory, hidePopover } from "./lib/api.js";
  import { costSummary } from "./lib/tiles.js";
  import { usd } from "./lib/format.js";
  import Usage from "./lib/Usage.svelte";
  import Settings from "./lib/Settings.svelte";

  let providers = $state([]);
  let snapshots = $state([]);
  let history = $state({});
  let config = $state(null);
  let cadenceSecs = $state(null);
  let view = $state("usage");
  let now = $state(Date.now());
  let refreshing = $state(false);
  // Sparkline window, "24h" or "7d". A view preference, not a setting: it lives here
  // (not in Usage) so it survives a round trip through Settings, and resets on restart.
  let sparkWindow = $state("24h");
  // Tile density, "wide" (full tiles) or "compact" (one line each). Same reasoning: a
  // view preference that outlives a Settings round trip.
  let density = $state("wide");

  const enabled = $derived(
    providers.filter((p) => config?.providers?.[p.id]?.enabled),
  );

  // Aggregated USD across every provider, shown in the header. null when nothing reports
  // dollars, so the header stays clean.
  const cost = $derived(costSummary(snapshots));
  const costLabel = $derived.by(() => {
    if (!cost) return "";
    const parts = [];
    if (cost.hasSpend) parts.push(`${usd(cost.spend)} spent`);
    if (cost.hasBalance) parts.push(`${usd(cost.balance)} balance`);
    return parts.join(" · ");
  });

  // Row 20: a tile is stale once it is older than two refresh intervals. One missed
  // cycle is normal; two means the number on screen is not what the tile implies.
  //
  // The interval comes from the backend, not from `refresh_minutes`: under the adaptive
  // cadence (the default for fresh installs) that field is only the fixed interval sitting
  // underneath, and an idle machine batches every 30 minutes. Sizing this from 5 minutes
  // would stamp "29m ago" on every tile while the data is exactly as current as the policy
  // in use intends. `tray::is_stale` reads the same number.
  const staleMs = $derived((cadenceSecs ?? 300) * 2 * 1000);

  async function loadConfig() {
    const [cfg, list, cadence] = await Promise.all([
      call("get_config"),
      call("list_providers"),
      call("get_cadence_secs"),
    ]);
    if (cfg) config = cfg;
    if (list) providers = list;
    if (typeof cadence === "number") cadenceSecs = cadence;
  }

  async function loadSnapshots() {
    const s = await call("get_snapshots");
    if (s) snapshots = s;
  }

  // Sparkline series, one array per provider id. Fetched separately from the
  // usage-updated payload so the event stays small.
  async function loadHistory() {
    const h = await getHistory();
    if (h) history = h;
  }

  async function saveConfig(next) {
    config = next;
    await call("set_config", { config: next });
    // The backend recomputes which providers count as configured, and switching between
    // adaptive and a fixed interval changes what counts as stale.
    await loadConfig();
  }

  // The theme choice ("auto" | "dark" | "light") resolves to a concrete scheme on the
  // <html> data-theme attribute. "auto" follows the OS via matchMedia and re-resolves
  // when the OS changes; the popover is hidden until JS has run, so there is no flash.
  const systemLight = matchMedia("(prefers-color-scheme: light)");
  function applyTheme(theme) {
    const resolved =
      theme === "light"
        ? "light"
        : theme === "dark"
          ? "dark"
          : systemLight.matches
            ? "light"
            : "dark";
    document.documentElement.dataset.theme = resolved;
  }
  $effect(() => applyTheme(config?.theme ?? "auto"));

  function cycleTheme() {
    const order = ["auto", "dark", "light"];
    const current = config?.theme ?? "auto";
    const next = order[(order.indexOf(current) + 1) % order.length];
    saveConfig({ ...config, theme: next });
  }

  async function refreshAll() {
    if (refreshing) return;
    refreshing = true;
    await call("refresh_all");
    await loadSnapshots();
    refreshing = false;
  }

  async function retry(id) {
    await call("refresh_provider", { id });
    await loadSnapshots();
  }

  onMount(() => {
    loadConfig();
    loadSnapshots();
    loadHistory();

    // Countdowns tick locally, no backend traffic.
    const tick = setInterval(() => (now = Date.now()), 30000);

    // Escape backs out of Settings, then closes the popover. Same muscle memory as
    // every other menu bar app.
    const onKey = (e) => {
      if (e.key !== "Escape") return;
      if (view === "settings") view = "usage";
      else hidePopover();
    };
    window.addEventListener("keydown", onKey);

    // In auto mode the OS can change scheme out from under us; re-resolve. A forced
    // choice ignores this, so the listener only matters while theme is "auto"/unset.
    const onScheme = () => applyTheme(config?.theme ?? "auto");
    systemLight.addEventListener("change", onScheme);

    const subs = [
      listen("usage-updated", (e) => {
        if (Array.isArray(e.payload)) snapshots = e.payload;
        loadHistory();
      }),
      listen("open-settings", () => (view = "settings")),
    ];

    return () => {
      clearInterval(tick);
      window.removeEventListener("keydown", onKey);
      systemLight.removeEventListener("change", onScheme);
      for (const s of subs) s.then((un) => un()).catch(() => {});
    };
  });
</script>

<div class="shell">
  <header data-tauri-drag-region>
    <!-- The whole identity of the app: an ascending meter, the thing every tile
         below is a variation of. Monochrome on purpose, this is a utility. -->
    <span class="mark" data-tauri-drag-region aria-hidden="true">
      <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
        <rect x="1.5" y="9" width="3" height="5" rx="1.5" opacity="0.34" />
        <rect x="6.5" y="5.5" width="3" height="8.5" rx="1.5" opacity="0.62" />
        <rect x="11.5" y="2" width="3" height="12" rx="1.5" />
      </svg>
    </span>
    <span class="title" data-tauri-drag-region>
      {view === "settings" ? "Settings" : "AgentsBar"}
    </span>
    <span class="gap" data-tauri-drag-region></span>
    {#if view === "settings"}
      <button
        class="iconbtn"
        title="Back"
        aria-label="Back to usage"
        onclick={() => (view = "usage")}
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6">
          <path d="M11.5 3.5 6 8l5.5 4.5" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    {:else if costLabel}
      <!-- Aggregated USD, up here rather than in the footer so the action strip stays
           uncluttered. Dim on purpose: it is context, not something to act on. -->
      <span
        class="cost"
        data-tauri-drag-region
        title="Aggregated from providers that report USD — OpenAI spend (last 30 days); DeepSeek, OpenRouter, xAI and OpenAI remaining balance"
      >
        {costLabel}
      </span>
    {/if}
  </header>

  {#if view === "settings"}
    <Settings {providers} {config} onSave={saveConfig} onKeySaved={loadConfig} />
  {:else}
    <Usage
      providers={enabled}
      {snapshots}
      {history}
      {config}
      {now}
      {refreshing}
      {staleMs}
      pinned={config?.pinned_provider ?? null}
      ready={config !== null}
      {sparkWindow}
      onSparkWindow={(w) => (sparkWindow = w)}
      {density}
      onDensity={(d) => (density = d)}
      onRefresh={refreshAll}
      onRetry={retry}
      onSettings={() => (view = "settings")}
      onThemeChange={cycleTheme}
      onQuit={() => call("quit_app")}
    />
  {/if}
</div>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  /* The window is undecorated, so this bar is the title bar. It earns that
     read from a 1px top sheen and a single vertical gradient, not from a blur
     stack: both are free at paint time and survive a 5 minute tick. */
  header {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex: none;
    height: 38px;
    padding: 0 var(--sp-3) 0 var(--sp-5);
    border-bottom: 1px solid var(--border);
    background: linear-gradient(
      180deg,
      var(--surface-chrome-top) 0%,
      var(--surface-chrome) 100%
    );
    box-shadow: inset 0 1px 0 var(--sheen);
  }

  .mark {
    display: flex;
    flex: none;
    color: var(--text-primary);
    /* Clicks land on the span, which carries the drag region attribute. */
    pointer-events: none;
  }

  .title {
    font-size: var(--type-body);
    font-weight: var(--weight-bold);
    letter-spacing: 0.2px;
    color: var(--text-primary);
  }

  .cost {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-meta);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .gap {
    flex: 1;
    align-self: stretch;
  }
</style>
