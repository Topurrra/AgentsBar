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

  // Quadratic through each point, landing on the midpoint of the next segment.
  // One pass, no control point solving, and it cannot overshoot the data. A run of one
  // point draws a round dot, which is what an isolated lane deserves.
  function curve(xy) {
    let d = `M${fmt(xy[0])}`;
    for (let i = 1; i < xy.length - 1; i++) {
      const mid = [(xy[i][0] + xy[i + 1][0]) / 2, (xy[i][1] + xy[i + 1][1]) / 2];
      d += ` Q${fmt(xy[i])} ${fmt(mid)}`;
    }
    return `${d} L${fmt(xy.at(-1))}`;
  }

  // x spreads the sample window over the full width, y is the used percent measured
  // down from the top, which is the remaining percent measured up from the bottom.
  //
  // Row 17: samples carry the lane label they were measured against (`l`). A weekly
  // series becoming a 5h series is a different quota, not a cliff, so the line breaks
  // there instead of drawing a drop that never happened. Samples written before the
  // label existed have `l === undefined` throughout, which is one unbroken run.
  //
  // The label alone is not enough to break on. It names whichever lane is currently the
  // most constrained, so it also flips when two lanes merely cross: 5h 49 / weekly 50
  // becomes 5h 51 / weekly 50 and the label changes while the value moved one point.
  // Breaking there fragments a perfectly smooth series. A cliff is a label change AND a
  // jump, so both are required.
  const CLIFF = 10; // percentage points
  const isCliff = (a, b) => a.l !== b.l && Math.abs(a.u - b.u) > CLIFF;
  const segments = $derived.by(() => {
    if (points.length < 2) return [];
    const t0 = points[0].t;
    // max, not `|| 1`: a clock step can leave the last timestamp at or before the first,
    // and a zero or negative span would push x outside the viewBox.
    const span = Math.max(1, points.at(-1).t - t0);
    const xy = points.map((s) => [
      inset + ((s.t - t0) / span) * (width - inset * 2),
      inset + (Math.min(100, Math.max(0, s.u)) / 100) * (height - inset * 2),
    ]);
    const runs = [];
    let start = 0;
    for (let i = 1; i <= points.length; i++) {
      if (i === points.length || isCliff(points[i], points[i - 1])) {
        runs.push(xy.slice(start, i));
        start = i;
      }
    }
    return runs.map(curve);
  });

  // Same thresholds as tone() in format.js, keyed off the newest remaining value.
  const stroke = $derived.by(() => {
    const left = 100 - points.at(-1)?.u;
    return left > 50 ? "var(--ok)" : left >= 20 ? "var(--warn)" : "var(--bad)";
  });
</script>

{#if segments.length}
  <svg class="spark" {width} {height} viewBox="0 0 {width} {height}" aria-hidden="true">
    {#each segments as d}
      <path {d} fill="none" {stroke} stroke-width="1.5" stroke-linecap="round"
        stroke-linejoin="round" />
    {/each}
  </svg>
{/if}

<style>
  .spark {
    display: block;
    opacity: 0.85;
  }
</style>
