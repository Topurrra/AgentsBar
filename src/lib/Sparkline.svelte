<script>
  // Percent REMAINING (100 - used) over time. No axes, no library, no dependency:
  // the shape is the message, the exact numbers live in the tile next to it.
  let { samples = [], width = 120, height = 22 } = $props();

  const inset = 1; // half the stroke width, so the line is never clipped
  const fmt = ([x, y]) => `${x.toFixed(1)} ${y.toFixed(1)}`;

  // history.rs appends in time order, so no sort here. Non-finite entries are dropped
  // because one NaN would break the whole path string.
  const points = $derived(
    (samples ?? []).filter((s) => Number.isFinite(s?.t) && Number.isFinite(s?.u)),
  );

  // x spreads the sample window over the full width, y is the used percent measured
  // down from the top, which is the remaining percent measured up from the bottom.
  const path = $derived.by(() => {
    if (points.length < 2) return "";
    const t0 = points[0].t;
    // max, not `|| 1`: a clock step can leave the last timestamp at or before the first,
    // and a zero or negative span would push x outside the viewBox.
    const span = Math.max(1, points.at(-1).t - t0);
    const xy = points.map((s) => [
      inset + ((s.t - t0) / span) * (width - inset * 2),
      inset + (Math.min(100, Math.max(0, s.u)) / 100) * (height - inset * 2),
    ]);
    // Quadratic through each point, landing on the midpoint of the next segment.
    // One pass, no control point solving, and it cannot overshoot the data.
    let d = `M${fmt(xy[0])}`;
    for (let i = 1; i < xy.length - 1; i++) {
      const mid = [(xy[i][0] + xy[i + 1][0]) / 2, (xy[i][1] + xy[i + 1][1]) / 2];
      d += ` Q${fmt(xy[i])} ${fmt(mid)}`;
    }
    return `${d} L${fmt(xy.at(-1))}`;
  });

  // Same thresholds as tone() in format.js, keyed off the newest remaining value.
  const stroke = $derived.by(() => {
    const left = 100 - points.at(-1)?.u;
    return left > 50 ? "var(--ok)" : left >= 20 ? "var(--warn)" : "var(--bad)";
  });
</script>

{#if path}
  <svg class="spark" {width} {height} viewBox="0 0 {width} {height}" aria-hidden="true">
    <path d={path} fill="none" {stroke} stroke-width="1.5" stroke-linecap="round"
      stroke-linejoin="round" />
  </svg>
{/if}

<style>
  .spark {
    display: block;
    opacity: 0.85;
  }
</style>
