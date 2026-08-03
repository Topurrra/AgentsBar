<script>
  import ProviderTile from "./ProviderTile.svelte";
  import ProviderCompact from "./ProviderCompact.svelte";
  import CostAdvice from "./CostAdvice.svelte";
  import Trends from "./Trends.svelte";
  import { clockTime } from "./format.js";
  import { sortTiles, oldestFetch, recommend, multiAccountProviders, outagesSince } from "./tiles.js";

  let {
    providers,
    snapshots,
    history = {},
    health = {},
    config = null,
    now,
    ready,
    refreshing,
    staleMs = 600000,
    sparkWindow = "24h",
    onSparkWindow,
    density = "compact",
    onDensity,
    onWidget,
    onRefresh,
    onRetry,
    onSettings,
    onThemeChange,
    onQuit,
  } = $props();

  const byId = $derived(new Map(snapshots.map((s) => [s.provider_id, s])));

  // Outages in the last 7 days, per provider id. Recomputed whenever health or the clock
  // tick changes; the tile shows it only when it is non-zero.
  const outages = $derived.by(() => {
    const since = now / 1000 - 7 * 86400;
    const map = {};
    for (const p of providers) {
      const n = outagesSince(health[p.id], since);
      if (n > 0) map[p.id] = n;
    }
    return map;
  });

  const providerOrder = $derived(config?.provider_order ?? []);
  const tiles = $derived(sortTiles(providers, byId, providerOrder));

  // The provider to reach for next (most headroom). Only surfaced when there is an actual
  // choice — with a single provider the tile already says everything.
  const rec = $derived(providers.length >= 2 ? recommend(providers, byId) : null);

  // Providers with more than one account in the history (each series key is one account,
  // row 35). Only for those does the active account deserve to be named on the tile: a
  // single-account provider printing its account everywhere is noise, the kind of clutter
  // "healthy recedes" exists to avoid.
  const multiAccount = $derived(multiAccountProviders(history));

  // Row 20: the stamp is the oldest of the snapshots actually on screen. A single
  // provider still refreshing must not let the footer claim the rest are current.
  const shown = $derived(providers.map((p) => byId.get(p.id)).filter(Boolean));
  const refreshed = $derived(clockTime(oldestFetch(shown)));

  // The sparkline window. The buffer holds ~7 days; this picks how much of it each
  // sparkline draws, filtering on the sample timestamps so the line fills its width
  // with exactly the chosen span.
  const windowSecs = $derived(sparkWindow === "7d" ? 7 * 86400 : 86400);
  const cutoff = $derived(now / 1000 - windowSecs);
  const samplesFor = (key) => (history[key] ?? []).filter((s) => s.t >= cutoff);

  // The theme button names the CURRENT scheme so the footer reads as a status, and the
  // tooltip names the next one so the click is predictable. auto resolves through the OS.
  const theme = $derived(config?.theme ?? "auto");
  const themeLabel = $derived(
    theme === "light" ? "Light" : theme === "dark" ? "Dark" : "Auto",
  );
  const themeNext = $derived(
    theme === "auto" ? "dark" : theme === "dark" ? "light" : "auto",
  );
</script>

<!-- With nothing to list, the scroller becomes a centring box: 400px of void above a
     message pinned to the top is the one state that looks unfinished rather than empty. -->
<div class="scroll" class:centred={!providers.length}>
  {#if providers.length}
    {#if density === "compact"}
      <div class="rows">
        {#each tiles as p (p.id)}
          <ProviderCompact
            provider={p}
            snapshot={byId.get(p.id)}
            cookieSource={config?.providers?.[p.id]?.cookie_source ?? null}
            recommended={rec?.provider.id === p.id}
            {now}
            {onRetry}
          />
        {/each}
        <CostAdvice {snapshots} />
        <Trends {history} {providers} {now} />
      </div>
    {:else}
      <div class="tiles">
        {#each tiles as p (p.id)}
          <!-- Row 35: history is keyed by SERIES (`provider:account`), not by provider id.
               The key is computed in Rust and travels on the snapshot, so the rule is not
               re-spelled here. The fallback covers the tick before the first snapshot. -->
          <ProviderTile
            provider={p}
            snapshot={byId.get(p.id)}
            samples={samplesFor(byId.get(p.id)?.history_key ?? p.id)}
            cookieSource={config?.providers?.[p.id]?.cookie_source ?? null}
            multi={multiAccount.has(p.id)}
            outages={outages[p.id] ?? 0}
            recommended={rec?.provider.id === p.id}
            {now}
            {staleMs}
            {onRetry}
          />
        {/each}
        <CostAdvice {snapshots} />
        <Trends {history} {providers} {now} />
      </div>
    {/if}
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
  {#if density === "wide"}
    <span class="seg" role="group" aria-label="Sparkline window">
      <button
        class:active={sparkWindow === "24h"}
        aria-pressed={sparkWindow === "24h"}
        onclick={() => onSparkWindow("24h")}
      >
        24h
      </button>
      <button
        class:active={sparkWindow === "7d"}
        aria-pressed={sparkWindow === "7d"}
        onclick={() => onSparkWindow("7d")}
      >
        7d
      </button>
    </span>
  {/if}
  <span class="gap"></span>

  <!-- Density toggle: the icon shows the view you will switch TO. -->
  <button
    class="iconbtn"
    title={density === "wide" ? "Compact view" : "Wide view"}
    aria-label={density === "wide" ? "Switch to compact view" : "Switch to wide view"}
    onclick={() => onDensity(density === "wide" ? "compact" : "wide")}
  >
    {#if density === "wide"}
      <!-- compact: three close thin lines -->
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
        <path d="M3 3.5h10M3 8h10M3 12.5h10" stroke-linecap="round" />
      </svg>
    {:else}
      <!-- wide: two spaced rounded cards -->
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
        <rect x="3" y="3" width="10" height="3.6" rx="1.2" />
        <rect x="3" y="9.4" width="10" height="3.6" rx="1.2" />
      </svg>
    {/if}
  </button>

  <button
    class="iconbtn"
    title="Toggle desktop widget"
    aria-label="Toggle desktop widget"
    onclick={onWidget}
  >
    <!-- a small widget tile with a meter inside -->
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
      <rect x="2.5" y="2.5" width="11" height="11" rx="2" />
      <path d="M5.5 10.5V8M8 10.5V6M10.5 10.5V9" stroke-linecap="round" />
    </svg>
  </button>

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

  <button
    class="iconbtn"
    title="Theme: {themeLabel} — click for {themeNext}"
    aria-label="Theme: {themeLabel}"
    onclick={onThemeChange}
  >
    {#if theme === "light"}
      <!-- sun -->
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
        <circle cx="8" cy="8" r="2.2" />
        <path
          d="M8 1.5v1.6M8 12.9v1.6M14.5 8h-1.6M3.1 8H1.5M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1M12.6 12.6l-1.1-1.1M4.5 4.5 3.4 3.4"
          stroke-linecap="round"
        />
      </svg>
    {:else if theme === "dark"}
      <!-- moon -->
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
        <path
          d="M13.4 9.6A5.6 5.6 0 0 1 6.4 2.6a5.6 5.6 0 1 0 7 7z"
          stroke-linejoin="round"
        />
      </svg>
    {:else}
      <!-- auto: half-filled circle -->
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4">
        <circle cx="8" cy="8" r="5.6" />
        <path d="M8 2.4a5.6 5.6 0 0 1 0 11.2z" fill="currentColor" stroke="none" />
      </svg>
    {/if}
  </button>

  <button class="iconbtn" title="Settings" aria-label="Settings" onclick={onSettings}>
    <!-- gear -->
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
      <circle cx="12" cy="12" r="3" />
      <path
        d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"
        stroke-linejoin="round"
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
    gap: var(--sp-3);
    /* 12px of air on both sides, which is also the header's left inset and Settings'
       card inset, so switching views does not shift the cards sideways. The right value
       is one step down because .scroll already reserves a stable 8px scrollbar gutter. */
    padding: var(--sp-4) var(--sp-2) var(--sp-4) var(--sp-5);
  }

  /* Compact density: one provider per line, tighter rhythm than the tiles but with real
     breathing room — dense, not cramped. Same side insets as .tiles so flipping density
     does not shift the list sideways. */
  .rows {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-3) var(--sp-2) var(--sp-3) var(--sp-5);
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

  /* Sparkline window picker: a quiet segmented control that reads as one object, the
     active span lifted with the same overlay the rest of the chrome uses on hover. */
  .seg {
    display: inline-flex;
    margin-left: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    overflow: hidden;
  }

  .seg button {
    padding: var(--sp-1) var(--sp-3);
    font-size: var(--type-meta);
    line-height: var(--leading-tight);
    color: var(--text-muted);
    transition:
      color var(--motion-fast) var(--ease),
      background var(--motion-fast) var(--ease);
  }

  .seg button:hover {
    color: var(--text-secondary);
  }

  .seg button.active {
    color: var(--text-primary);
    background: var(--overlay-hover);
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
