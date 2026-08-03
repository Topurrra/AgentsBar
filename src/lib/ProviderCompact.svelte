<script>
  // The compact row: one provider per line for scanning many at once. It trades the
  // tile's detail (every lane, pace, sparkline) for density: icon, name, the binding
  // window as a thin bar, the percent, and the reset. Same state ramp as the tile, so a
  // list of rows reads for trouble exactly the way the tiles do.
  import { percentLeft, tone, countdown } from "./format.js";
  import { windowsOf, errorCopy } from "./tiles.js";
  import { providerAccent } from "./icons.js";
  import { openUrl } from "./api.js";
  import ProviderIcon from "./ProviderIcon.svelte";

  let { provider, snapshot, now, cookieSource = null, recommended = false, onRetry } = $props();

  const fail = $derived(errorCopy(provider, snapshot, cookieSource));
  const retryable = $derived(!!fail?.retry && typeof onRetry === "function");
  const windows = $derived(windowsOf(snapshot));

  const RAMP = { ok: "calm", warn: "watch", bad: "low", unknown: "unknown" };
  const step = (left) => (left === 0 ? "spent" : RAMP[tone(left)]);

  // The constraining lane: least percent left, ties to the earlier lane.
  const binding = $derived.by(() => {
    let at = windows.length ? 0 : -1;
    let min = Infinity;
    windows.forEach((w, i) => {
      const left = percentLeft(w);
      if (left !== null && left < min) {
        min = left;
        at = i;
      }
    });
    return at;
  });

  const lead = $derived(binding >= 0 ? windows[binding] : null);
  const left = $derived(percentLeft(lead));

  // A failure outranks the last good numbers. A bad failure is red; a transient blip
  // stays muted so it does not read as trouble.
  const state = $derived.by(() => {
    if (fail) return fail.tone === "bad" ? "low" : "unknown";
    if (!lead) return "unknown";
    return step(left);
  });

  const accentSoft = $derived(providerAccent(provider.id) + "40");

  function activate() {
    if (retryable) onRetry(provider.id);
    else openUrl(provider.doc_url);
  }
</script>

<button
  class="row {state}"
  class:rec={recommended}
  style="--accent-soft: {accentSoft}"
  title={retryable ? "Retry " + provider.name : (snapshot?.account ?? provider.name)}
  aria-label={retryable ? "Retry " + provider.name : (snapshot?.account ?? provider.name)}
  onclick={activate}
  disabled={!retryable && !provider.doc_url}
>
  <ProviderIcon id={provider.id} size={15} />
  <span class="identity">
    <span class="name">{provider.name}</span>
    {#if recommended}
      <span class="rec-tag">most room</span>
    {/if}
  </span>

  {#if fail}
    <span class="detail err {fail.tone}" title={snapshot.error}>{fail.text}</span>
  {:else if lead}
    <span class="bar" aria-hidden="true">
      <span class="fill" style="--v: {(left ?? 100) / 100}"></span>
    </span>
    <span class="pct">{left === null ? "?" : left + "%"}</span>
    <span class="reset">
      {lead.resets_at ? countdown(lead.resets_at, now) : ""}
    </span>
  {:else}
    <span class="detail none">no data</span>
  {/if}
</button>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-4);
    border-radius: var(--radius-md);
    text-align: left;
    background: linear-gradient(135deg, var(--overlay-hover), var(--surface-float-raised));
    border: 1px solid var(--border);
    /* Brand is identity only; urgency remains on the meter and percentage. */
    box-shadow: inset 2px 0 0 var(--accent-soft);
    transition:
      background var(--motion-fast) var(--ease),
      border-color var(--motion-fast) var(--ease);
  }

  .row:not(:disabled):hover {
    background: linear-gradient(135deg, var(--overlay-hover), var(--surface-raised-hover));
    border-color: var(--border-strong);
  }

  .row:disabled {
    cursor: default;
  }

  .row.rec {
    border-color: var(--state-calm-bar);
    box-shadow: inset 2px 0 0 var(--state-calm-bar);
  }

  .identity {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    flex: 1 1 0;
    min-width: 0;
  }

  .name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
  }

  .rec-tag {
    flex: none;
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--chip-bg);
    color: var(--state-calm-text);
    font-size: var(--type-meta);
    line-height: var(--leading-tight);
    white-space: nowrap;
  }

  /* The bar and the percent carry the state colour; the name stays neutral and the
     reset stays muted, so colour means the same thing it means on the tile. */
  .bar {
    flex: none;
    width: 52px;
    height: 4px;
    border-radius: var(--radius-pill);
    background: var(--surface-track);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: currentColor;
    transform-origin: left;
    transform: scaleX(var(--v, 1));
    transition: transform var(--motion-slow) var(--ease);
  }

  .pct {
    flex: none;
    width: 34px;
    text-align: right;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
  }

  .reset {
    flex: none;
    width: 46px;
    text-align: right;
    font-size: var(--type-meta);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .detail {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-meta);
  }

  .row > :last-child {
    min-width: 0;
  }

  .err.bad {
    color: var(--state-low-text);
  }

  .err.warn {
    color: var(--state-watch-text);
  }

  .err.muted {
    color: var(--text-secondary);
  }

  .none {
    color: var(--text-muted);
  }

  .row.calm {
    color: var(--state-calm-bar);
  }
  .row.watch {
    color: var(--state-watch-bar);
  }
  .row.low {
    color: var(--state-low-bar);
  }
  .row.spent {
    color: var(--state-spent-bar);
  }
  .row.unknown {
    color: var(--surface-hatch);
  }
</style>
