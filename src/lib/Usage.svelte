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

<!-- With nothing to list, the scroller becomes a centring box: 400px of void above a
     message pinned to the top is the one state that looks unfinished rather than empty. -->
<div class="scroll" class:centred={!providers.length}>
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
      <button class="btn cta" onclick={onSettings}>Open Settings</button>
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

  <button class="iconbtn quit" title="Quit AgentsBar" aria-label="Quit AgentsBar" onclick={onQuit}>
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
    gap: var(--sp-4);
    /* 12px of air on both sides, which is also the header's left inset and Settings'
       card inset, so switching views does not shift the cards sideways. The right value
       is one step down because .scroll already reserves a stable 8px scrollbar gutter. */
    padding: var(--sp-4) var(--sp-2) var(--sp-4) var(--sp-5);
  }

  .centred {
    display: flex;
    flex-direction: column;
    justify-content: center;
  }

  .empty {
    /* Optically centred: a block of text sits high in its box, so the bottom padding
       carries the extra rather than the maths. */
    padding: var(--sp-7) var(--sp-7) calc(var(--sp-7) + var(--sp-6));
    text-align: center;
    color: var(--text-muted);
  }

  .empty p {
    margin: 0 0 var(--sp-3);
    line-height: var(--leading-body);
  }

  .lead {
    color: var(--text-primary);
    font-weight: var(--weight-medium);
  }

  /* The shared .btn at the one size in the app that is a primary action rather than a
     utility, so it takes body type and a roomier box. */
  .cta {
    margin-top: var(--sp-5);
    padding: var(--sp-3) var(--sp-5);
    font-size: var(--type-body);
    --btn-fg: var(--text-primary);
    --btn-fill: none;
  }

  /* Mirrors the header: the window is undecorated, so the footer has to look like a
     built edge rather than the page simply ending. Same 38px as the header, so the two
     chrome bars are the same object at the two ends of the window. */
  footer {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    flex: none;
    height: 38px;
    padding: 0 var(--sp-3);
    border-top: 1px solid var(--border);
    background: linear-gradient(
      180deg,
      var(--surface-chrome) 0%,
      var(--surface-app) 100%
    );
  }

  .stamp {
    padding-left: var(--sp-3);
    font-size: var(--type-meta);
    color: var(--text-muted);
  }

  .gap {
    flex: 1;
  }

  .quit:hover {
    color: var(--state-low-text);
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
