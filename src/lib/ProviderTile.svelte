<script>
  import { percentLeft, tone, countdown, credits } from "./format.js";
  import { windowsOf, ago } from "./tiles.js";
  import { providerAccent } from "./icons.js";
  import ProviderIcon from "./ProviderIcon.svelte";
  import Sparkline from "./Sparkline.svelte";

  let { provider, snapshot, samples = [], now, staleMs = 600000, onRetry } = $props();

  // Row 25: the backend already applied the cap (an exhausted longer window binds every
  // shorter one) so the tray and the tile cannot disagree about it.
  const windows = $derived(windowsOf(snapshot));
  const balance = $derived(credits(snapshot?.credits));

  // Row 20: say how old this tile is once it is past two refresh intervals. Silence
  // below that, or every tile carries noise.
  const stale = $derived.by(() => {
    const t = Date.parse(snapshot?.fetched_at ?? "");
    return Number.isFinite(t) && now - t > staleMs ? ago(snapshot.fetched_at, now) : "";
  });
  // 8 digit hex: the brand color at low alpha, no color-mix needed.
  const accentSoft = $derived(providerAccent(provider.id) + "40");
</script>

<div class="tile" class:failed={!!snapshot?.error} style="--accent-soft: {accentSoft}">
  <div class="head">
    <ProviderIcon id={provider.id} size={15} />
    <span class="name">{provider.name}</span>
    {#if snapshot?.plan}<span class="badge">{snapshot.plan}</span>{/if}
    <span class="gap"></span>
    {#if snapshot?.account}
      <span class="account" title={snapshot.account}>{snapshot.account}</span>
    {/if}
    {#if stale}
      <span class="stale" title="Last fetched {snapshot.fetched_at}">{stale}</span>
    {/if}
    <Sparkline {samples} width={52} height={14} />
  </div>

  {#if snapshot?.error}
    <div class="error">
      <span class="msg" title={snapshot.error}>{snapshot.error}</span>
      <button class="retry" onclick={() => onRetry(provider.id)}>Retry</button>
    </div>
  {/if}

  <!-- Deliberately unkeyed: window labels are derived from the window length and two
       lanes of the same length share one. Row 36's keying is on provider_id, where the
       identity is real. -->
  {#each windows as w}
    {@const left = percentLeft(w)}
    <div class="win">
      <div class="winhead">
        <span class="label">{w.label}</span>
        <span class="gap"></span>
        <span class="pct {tone(left)}">{left === null ? "unknown" : left + "% left"}</span>
        {#if w.resets_at}
          <span class="reset">{countdown(w.resets_at, now)}</span>
        {/if}
      </div>
      <!-- An unknown window fills the track with a hatch, never a zero-width bar that
           reads as an untouched quota. -->
      <div
        class="track"
        role="progressbar"
        aria-label="{provider.name} {w.label} remaining"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={left ?? undefined}
        aria-valuetext={left === null ? "unknown" : left + "% left"}
      >
        <div class="fill {tone(left)}" style="width: {left ?? 100}%"></div>
      </div>
      {#if w.capped_by}
        <div class="capnote">Capped by {w.capped_by}</div>
      {/if}
    </div>
  {/each}

  {#if balance !== null}
    <div class="credits">Credits <b>{balance}</b></div>
  {/if}

  {#if !snapshot && !windows.length}
    <div class="idle">
      {#if provider.configured}
        Waiting for first refresh
      {:else if provider.auth === "cookie"}
        No session found. Sign in to {provider.name} in your browser, or paste a session
        in Settings.
      {:else}
        Not configured
      {/if}
    </div>
  {:else if !windows.length && !snapshot?.error && balance === null}
    <div class="idle">No usage reported</div>
  {/if}
</div>

<style>
  .tile {
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--panel);
    padding: 9px 10px 10px 11px;
    /* One hairline of the brand color down the left edge. That is the whole accent. */
    box-shadow: inset 2px 0 0 var(--accent-soft);
  }

  .tile.failed {
    border-color: rgba(229, 83, 75, 0.35);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .head :global(.spark) {
    margin-left: 2px;
  }

  .gap {
    flex: 1;
  }

  .name {
    font-weight: 600;
    letter-spacing: 0.1px;
  }

  .badge {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 1px 5px;
  }

  .account {
    font-size: 11px;
    color: var(--faint);
    max-width: 120px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .error {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 7px;
    font-size: 11.5px;
    color: var(--bad);
  }

  .msg {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .retry {
    flex: none;
    color: var(--bad);
    border: 1px solid rgba(229, 83, 75, 0.4);
    border-radius: 5px;
    padding: 1px 7px;
    font-size: 11px;
    transition: background 0.12s ease;
  }

  .retry:hover {
    background: rgba(229, 83, 75, 0.14);
  }

  .win {
    margin-top: 8px;
  }

  .winhead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 11.5px;
    margin-bottom: 4px;
  }

  .label {
    color: var(--dim);
  }

  .pct {
    font-variant-numeric: tabular-nums;
  }

  .reset {
    color: var(--faint);
    font-variant-numeric: tabular-nums;
    min-width: 42px;
    text-align: right;
  }

  .stale {
    font-size: 10.5px;
    color: var(--warn);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .track {
    height: 5px;
    border-radius: 3px;
    background: var(--track);
    overflow: hidden;
  }

  .capnote {
    margin-top: 4px;
    font-size: 10.5px;
    color: var(--faint);
  }

  .fill {
    height: 100%;
    border-radius: 3px;
    transition: width 0.25s ease;
  }

  .ok {
    color: var(--ok);
  }
  .warn {
    color: var(--warn);
  }
  .bad {
    color: var(--bad);
  }
  .unknown {
    color: var(--faint);
  }

  .fill.ok {
    background: var(--ok);
  }
  .fill.warn {
    background: var(--warn);
  }
  .fill.bad {
    background: var(--bad);
  }
  /* Hatched, not solid: a full flat bar in any colour would read as a measurement. */
  .fill.unknown {
    background: repeating-linear-gradient(
      -45deg,
      var(--hatch) 0 3px,
      transparent 3px 6px
    );
  }

  .credits {
    margin-top: 8px;
    font-size: 11.5px;
    color: var(--dim);
  }

  .credits b {
    color: var(--text);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .idle {
    margin-top: 7px;
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--faint);
  }
</style>
