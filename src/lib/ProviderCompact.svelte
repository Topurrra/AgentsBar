<script>
  // The compact row: one provider per line for scanning many at once. It trades the
  // tile's detail (every lane, pace, sparkline) for density — icon, name, the binding
  // window as a thin bar, the percent, and the reset. Same state ramp as the tile, so a
  // list of rows reads for trouble exactly the way the tiles do.
  import { percentLeft, tone, countdown } from "./format.js";
  import { windowsOf, errorCopy } from "./tiles.js";
  import { providerAccent } from "./icons.js";
  import { openUrl } from "./api.js";
  import ProviderIcon from "./ProviderIcon.svelte";

  let { provider, snapshot, now, cookieSource = null } = $props();

  const fail = $derived(errorCopy(provider, snapshot, cookieSource));
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
</script>

<button
  class="row {state}"
  style="--accent-soft: {accentSoft}"
  title={snapshot?.account ?? provider.name}
  onclick={() => openUrl(provider.doc_url)}
  disabled={!provider.doc_url}
>
  <ProviderIcon id={provider.id} size={15} />
  <span class="name">{provider.name}</span>

  {#if fail}
    <span class="detail err" title={snapshot.error}>{fail.text}</span>
  {:else if lead}
    <span class="bar" aria-hidden="true">
      <span class="fill" style="--v: {(left ?? 100) / 100}"></span>
    </span>
    <span class="pct">{left === null ? "—" : left + "%"}</span>
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
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--radius-md);
    text-align: left;
    /* A real card, not a bare line: the raised surface and hairline make each row read
       as its own object in both themes, exactly like the wide tile it condenses. */
    background: var(--surface-raised);
    border: 1px solid var(--border);
    /* One hairline of the brand color down the left, same accent as the wide tile. */
    box-shadow: inset 2px 0 0 var(--accent-soft);
    transition:
      background var(--motion-fast) var(--ease),
      border-color var(--motion-fast) var(--ease);
  }

  .row:not(:disabled):hover {
    background: var(--surface-raised-hover);
    border-color: var(--border-strong);
  }

  .row:disabled {
    cursor: default;
  }

  .name {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
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

  .err {
    color: var(--state-low-text);
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
