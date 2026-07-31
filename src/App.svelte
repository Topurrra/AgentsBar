<script>
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { call } from "./lib/api.js";
  import Usage from "./lib/Usage.svelte";
  import Settings from "./lib/Settings.svelte";

  let providers = $state([]);
  let snapshots = $state([]);
  let config = $state(null);
  let view = $state("usage");
  let now = $state(Date.now());
  let refreshing = $state(false);

  const enabled = $derived(
    providers.filter((p) => config?.providers?.[p.id]?.enabled),
  );

  async function loadConfig() {
    const [cfg, list] = await Promise.all([call("get_config"), call("list_providers")]);
    if (cfg) config = cfg;
    if (list) providers = list;
  }

  async function loadSnapshots() {
    const s = await call("get_snapshots");
    if (s) snapshots = s;
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

    // Countdowns tick locally, no backend traffic.
    const tick = setInterval(() => (now = Date.now()), 30000);
    const subs = [
      listen("usage-updated", (e) => {
        if (Array.isArray(e.payload)) snapshots = e.payload;
      }),
      listen("open-settings", () => (view = "settings")),
    ];

    return () => {
      clearInterval(tick);
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
      {now}
      {refreshing}
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
    background: #131316;
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
