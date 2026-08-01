<script>
  // Percent REMAINING (100 - used) over time. No axes, no library, no dependency:
  // the shape is the message, the exact numbers live in the tile above it.
  //
  // Wave 6: this used to be a 52px hairline wedged into the tile header, where it read
  // as a stray border. It now spans the foot of the tile at full width. The viewBox
  // width is just a coordinate space; the svg stretches to whatever the tile is, and
  // the stroke stays 1.25 device pixels because of vector-effect.
  //
  // Width is what makes it legible, so height stays small: a 14px band is a trend, a
  // 20px tinted slab was the biggest object on a tile whose whole point is to be quiet
  // when there is nothing to report. The area under the line arrives with trouble only,
  // the same way weight and colour do everywhere else.
  let { samples = [], height = 14 } = $props();

  const W = 120; // internal coordinate space, not a rendered width
  const inset = 1; // keeps the stroke off the edges of the box
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
  // down from the top, which is the remaining percent measured up from the bottom. So
  // the region between the line and the bottom edge IS the headroom, and tinting it is
  // the honest fill rather than decoration.
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
  // Under this much movement across the whole series there is no trend to draw, and a
  // dead flat line spanning the tile reads as a stray rule, which is exactly the thing
  // this component was redesigned to stop being. A provider that has not moved all week
  // says so by having no chart at all.
  // A series that barely moves draws a straight line, and a straight line inside a 14px
  // band reads as a stray rule under the tile rather than as data. Below this much
  // movement there is no trend worth a chart, so draw nothing.
  const FLAT = 4; // percentage points

  const segments = $derived.by(() => {
    if (points.length < 2) return [];
    const us = points.map((p) => p.u);
    if (Math.max(...us) - Math.min(...us) < FLAT) return [];
    const t0 = points[0].t;
    // max, not `|| 1`: a clock step can leave the last timestamp at or before the first,
    // and a zero or negative span would push x outside the viewBox.
    const span = Math.max(1, points.at(-1).t - t0);
    const xy = points.map((s) => [
      inset + ((s.t - t0) / span) * (W - inset * 2),
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
    // The line and the closed area under it. Closing per run, not once for the whole
    // series, so a cliff leaves a real gap instead of a bridge nobody measured.
    return runs.map((run) => {
      const line = curve(run);
      return {
        line,
        area: `${line} L${run.at(-1)[0].toFixed(1)} ${height} L${run[0][0].toFixed(1)} ${height} Z`,
      };
    });
  });

  // Same ramp as the tile, keyed off the newest remaining value: a full provider's
  // history is a quiet grey line, not a green one.
  const state = $derived.by(() => {
    if (!points.length) return "calm";
    const left = 100 - points.at(-1).u;
    if (left <= 0) return "spent";
    if (left < 20) return "low";
    if (left <= 50) return "watch";
    return "calm";
  });
</script>

{#if segments.length}
  <svg
    class="spark {state}"
    {height}
    viewBox="0 0 {W} {height}"
    preserveAspectRatio="none"
    aria-hidden="true"
  >
    {#each segments as s}
      <path class="area" d={s.area} />
      <path class="line" d={s.line} />
    {/each}
  </svg>
{/if}

<style>
  /* Stretches to the tile; preserveAspectRatio="none" plus non-scaling strokes means
     the horizontal squash never thins the line. */
  .spark {
    display: block;
    width: 100%;
  }

  .spark.calm {
    color: var(--state-calm-bar);
  }
  .spark.watch {
    color: var(--state-watch-bar);
  }
  .spark.low {
    color: var(--state-low-bar);
  }
  .spark.spent {
    color: var(--state-spent-bar);
  }

  .line {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.25;
    stroke-linecap: round;
    stroke-linejoin: round;
    vector-effect: non-scaling-stroke;
  }

  /* Mass arrives with trouble, exactly like weight and colour do in the state ramp. A
     calm or watch trend is a line; a lane that is nearly or fully spent gets the region
     under it filled, so a tile in difficulty gains ink instead of a tile at rest. */
  .area {
    fill: none;
  }

  .spark.low .area,
  .spark.spent .area {
    fill: currentColor;
    fill-opacity: 0.14;
  }
</style>
