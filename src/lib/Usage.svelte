<script>
  import ProviderTile from "./ProviderTile.svelte";
  import { clockTime } from "./format.js";
  import { sortTiles, oldestFetch } from "./tiles.js";

  let {
    providers,
    snapshots,
    history = {},
    config = null,
    now,
    ready,
    refreshing,
    staleMs = 600000,
    pinned = null,
    onRefresh,
    onRetry,
    onSettings,
    onQuit,
  } = $props();

  const byId = $derived(new Map(snapshots.map((s) => [s.provider_id, s])));

  // Row 15: most urgent first, so the reason you opened the popover is above the fold.
  const tiles = $derived(sortTiles(providers, byId, pinned));

  // Row 20: the stamp is the oldest of the snapshots actually on screen. A single
  // provider still refreshing must not let the footer claim the rest are current.
  const shown = $derived(providers.map((p) => byId.get(p.id)).filter(Boolean));
  const refreshed = $derived(clockTime(oldestFetch(shown)));
</script>

<div class="scroll">
  {#if providers.length}
    <div class="tiles">
      {#each tiles as p (p.id)}
        <!-- Row 35: history is keyed by SERIES (`provider:account`), not by provider id.
             The key is computed in Rust and travels on the snapshot, so the rule is not
             re-spelled here. The fallback covers the tick before the first snapshot. -->
        <ProviderTile
          provider={p}
          snapshot={byId.get(p.id)}
          samples={history[byId.get(p.id)?.history_key ?? p.id] ?? []}
          cookieSource={config?.providers?.[p.id]?.cookie_source ?? null}
          {now}
          {staleMs}
          {onRetry}
        />
      {/each}
    </div>
  {:else if ready}
    <div class="empty">
      <p class="lead">No providers enabled</p>
      <p>Open Settings to enable a provider or paste an API key.</p>
      <button class="cta" onclick={onSettings}>Open Settings</button>
    </div>
  {/if}
</div>

<footer>
  <span class="stamp" title="Oldest snapshot on screen">Updated {refreshed}</span>
  <span class="gap"></span>

  <button
    class="iconbtn"
    class:spin={refreshing}
    title="Refresh now"
    aria-label="Refresh now"
    onclick={onRefresh}
  >
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
      <path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9" stroke-linecap="round" />
      <path d="M13.5 2v3.2h-3.2" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  </button>

  <button class="iconbtn" title="Settings" aria-label="Settings" onclick={onSettings}>
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
      <circle cx="8" cy="8" r="2.2" />
      <path
        d="M8 1.5v1.6M8 12.9v1.6M14.5 8h-1.6M3.1 8H1.5M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1M12.6 12.6l-1.1-1.1M4.5 4.5 3.4 3.4"
        stroke-linecap="round"
      />
    </svg>
  </button>

  <button class="iconbtn quit" title="Quit AgentBar" aria-label="Quit AgentBar" onclick={onQuit}>
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
      <path d="M8 2v6" stroke-linecap="round" />
      <path d="M11.8 4.4a5 5 0 1 1-7.6 0" stroke-linecap="round" />
    </svg>
  </button>
</footer>

<style>
  .tiles {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
  }

  .empty {
    padding: 40px 26px;
    text-align: center;
    color: var(--faint);
  }

  .empty p {
    margin: 0 0 6px;
    line-height: 1.5;
  }

  .lead {
    color: var(--text);
    font-weight: 600;
  }

  .cta {
    margin-top: 10px;
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 5px 12px;
    color: var(--text);
    transition: background 0.12s ease;
  }

  .cta:hover {
    background: rgba(255, 255, 255, 0.07);
  }

  footer {
    display: flex;
    align-items: center;
    gap: 2px;
    flex: none;
    padding: 6px 8px;
    border-top: 1px solid var(--line);
    background: var(--chrome);
  }

  .stamp {
    padding-left: 4px;
    font-size: 11px;
    color: var(--faint);
    font-variant-numeric: tabular-nums;
  }

  .gap {
    flex: 1;
  }

  .quit:hover {
    color: var(--bad);
  }

  .spin svg {
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
