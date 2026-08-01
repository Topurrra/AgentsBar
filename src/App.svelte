<script>
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { call, getHistory, hidePopover } from "./lib/api.js";
  import Usage from "./lib/Usage.svelte";
  import Settings from "./lib/Settings.svelte";

  let providers = $state([]);
  let snapshots = $state([]);
  let history = $state({});
  let config = $state(null);
  let view = $state("usage");
  let now = $state(Date.now());
  let refreshing = $state(false);

  const enabled = $derived(
    providers.filter((p) => config?.providers?.[p.id]?.enabled),
  );

  // Row 20: a tile is stale once it is older than two refresh intervals. One missed
  // cycle is normal; two means the number on screen is not what the tile implies.
  const staleMs = $derived((config?.refresh_minutes ?? 5) * 2 * 60000);

  async function loadConfig() {
    const [cfg, list] = await Promise.all([call("get_config"), call("list_providers")]);
    if (cfg) config = cfg;
    if (list) providers = list;
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
    // The backend recomputes which providers count as configured.
    const list = await call("list_providers");
    if (list) providers = list;
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
      for (const s of subs) s.then((un) => un()).catch(() => {});
    };
  });
</script>

<div class="shell">
  <header data-tauri-drag-region>
    <span class="title" data-tauri-drag-region>
      {view === "settings" ? "Settings" : "AgentBar"}
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
    {/if}
  </header>

  {#if view === "settings"}
    <Settings {providers} {config} onSave={saveConfig} onKeySaved={loadConfig} />
  {:else}
    <Usage
      providers={enabled}
      {snapshots}
      {history}
      {now}
      {refreshing}
      {staleMs}
      pinned={config?.pinned_provider ?? null}
      ready={config !== null}
      onRefresh={refreshAll}
      onRetry={retry}
      onSettings={() => (view = "settings")}
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

  header {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
    height: 34px;
    padding: 0 6px 0 12px;
    border-bottom: 1px solid var(--line);
    background: var(--chrome);
  }

  .title {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.3px;
    color: var(--dim);
  }

  .gap {
    flex: 1;
    align-self: stretch;
  }
</style>
