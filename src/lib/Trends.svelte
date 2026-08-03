<script>
  // The Advisor block: week-over-week movement and run-out forecasts per provider, read
  // straight from the history buffer (see weekOverWeek / forecast in trends.js). The
  // block only exists when something is moving — a provider holding still is exactly the
  // healthy case "healthy recedes" covers, so with nothing to report it renders nothing
  // at all, and its absence IS the "all quiet" signal.
  import { weekOverWeek, forecast, samplesFor } from "./trends.js";
  import { countdown } from "./format.js";

  // `now` is epoch milliseconds like everywhere else in the app; the trend maths divides
  // it down to the samples' unix-seconds domain.
  let { history = {}, providers = [], now = Date.now() } = $props();

  const secs = $derived(now / 1000);

  // Most-notable first. A run-out date is the loudest thing a trend can say, so it
  // outranks everything; after that, rising usage sorts by how much it is rising, and
  // good news sorts under bad. Array.prototype.sort is stable, so equal notability keeps
  // registry order.
  const notability = (r) =>
    (r.eta !== null ? 1000 : 0) +
    (r.wow
      ? (r.wow.direction === "up" ? 100 : r.wow.direction === "down" ? 1 : 0) +
        Math.abs(r.wow.deltaPct)
      : 0);

  const rows = $derived.by(() => {
    const list = [];
    for (const p of providers ?? []) {
      if (!p?.configured) continue;
      const samples = samplesFor(history, p.id);
      const wow = weekOverWeek(samples, secs);
      const eta = forecast(samples, secs);
      // A flat week-over-week is deliberately not a signal: it says the provider held
      // still, which is nothing to report. A row needs a real movement or a run-out date.
      const hasSignal = (wow && wow.direction !== "flat") || eta !== null;
      if (!hasSignal) continue;
      list.push({ provider: p, wow, eta });
    }
    return list.sort((a, b) => notability(b) - notability(a));
  });

  // Rising usage earns the ramp's amber, and a steep climb — 20 points or more in a week
  // — escalates to its red. Falling usage is the good kind of movement: calm green.
  const wowTone = (wow) =>
    wow.direction === "down" ? "calm" : Math.abs(wow.deltaPct) >= 20 ? "low" : "watch";

  // A run-out date reads louder the closer it is: inside two days is red, further is amber.
  const etaTone = (eta) => (eta - now < 2 * 86400000 ? "low" : "watch");

  // The countdown formatter speaks the app's "2d 4h" dialect; the tilde says the number
  // is a projection, not a measurement.
  const etaText = (eta) => {
    const left = countdown(new Date(eta).toISOString(), now);
    return left === "now" ? "now" : `in ~${left}`;
  };
</script>

{#if rows.length}
  <section class="trends" aria-label="Usage trends">
    <span class="tag">Trends</span>
    {#each rows as r (r.provider.id)}
      {@const wowVisible = r.wow && r.wow.direction !== "flat"}
      <div class="row">
        <span class="name">{r.provider.name}</span>
        <span class="sig">
          {#if wowVisible}
            <span class="wow {wowTone(r.wow)}">
              {r.wow.direction === "up" ? "↑" : "↓"}
              {Math.round(Math.abs(r.wow.deltaPct))}% week-over-week
            </span>
          {/if}
          {#if wowVisible && r.eta !== null}
            <span class="sep">·</span>
          {/if}
          {#if r.eta !== null}
            <span class="eta {etaTone(r.eta)}">hits limit {etaText(r.eta)}</span>
          {/if}
        </span>
      </div>
    {/each}
  </section>
{/if}

<style>
  .trends {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--radius-md);
    background: var(--surface-raised);
    border: 1px solid var(--border);
  }

  /* Same micro-label treatment the lane labels get: context, not content. */
  .tag {
    font-size: var(--type-meta);
    line-height: var(--leading-tight);
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
    color: var(--text-muted);
  }

  .row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
  }

  .name {
    flex: none;
    /* Long provider names ellipsize rather than push the signal onto a second line. */
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
  }

  /* The sentence about the provider. Dim by default; the coloured spans inside carry
     whatever the state ramp says is worth colour. */
  .sig {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--sp-2);
    font-size: var(--type-meta);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .wow.watch,
  .eta.watch {
    color: var(--state-watch-text);
  }

  .wow.low,
  .eta.low {
    color: var(--state-low-text);
  }

  .wow.calm {
    color: var(--state-calm-text);
  }

  .sep {
    opacity: 0.55;
  }
</style>
