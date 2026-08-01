<script>
  import { percentLeft, tone, countdown, credits } from "./format.js";
  import { windowsOf, ago, pace, errorCopy } from "./tiles.js";
  import { providerAccent } from "./icons.js";
  import ProviderIcon from "./ProviderIcon.svelte";
  import Sparkline from "./Sparkline.svelte";

  let {
    provider,
    snapshot,
    samples = [],
    now,
    staleMs = 600000,
    cookieSource = null,
    onRetry,
  } = $props();

  // Rows 21 and 22. The wording and the tone both come from `error_kind`, never from the
  // message text. `fail` is null whenever there is nothing wrong.
  const fail = $derived(errorCopy(provider, snapshot, cookieSource));

  // Row 25: the backend already applied the cap (an exhausted longer window binds every
  // shorter one) so the tray and the tile cannot disagree about it.
  const windows = $derived(windowsOf(snapshot));
  const balance = $derived(credits(snapshot?.credits));

  // Wave 6 state ramp. `tone()` still owns the thresholds; the only step added here is
  // `spent`, which app.css defines for a window with nothing left at all. The names map
  // one to one onto the --state-*-bar/-text/-edge/-weight groups in app.css.
  const RAMP = { ok: "calm", warn: "watch", bad: "low", unknown: "unknown" };
  const step = (left) => (left === 0 ? "spent" : RAMP[tone(left)]);

  // The lane that actually constrains you: the lowest percent left. Ties go to the
  // earlier lane because the backend sends the primary window first.
  //
  // A tile with lanes always has exactly one hero. When no lane carries a number the
  // first one is it: the primary window is still the thing the tile is about, and an
  // unknown hero is muted grey at regular weight anyway, so promoting it costs nothing
  // in loudness and saves a one-lane tile from rendering with no focal point at all.
  // -1 only when there are no lanes to choose from.
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

  // The tile frames itself from the binding lane, so a list of tiles can be scanned for
  // trouble without reading a single number. Calm and unknown resolve to `transparent`
  // in app.css, so those tiles keep the plain hairline.
  const tileState = $derived(
    binding < 0 ? "unknown" : step(percentLeft(windows[binding])),
  );

  // Row 20: say how old this tile is once it is past two refresh intervals. Silence
  // below that, or every tile carries noise.
  const stale = $derived.by(() => {
    const t = Date.parse(snapshot?.fetched_at ?? "");
    return Number.isFinite(t) && now - t > staleMs ? ago(snapshot.fetched_at, now) : "";
  });
  // 8 digit hex: the brand color at low alpha, no color-mix needed.
  const accentSoft = $derived(providerAccent(provider.id) + "40");
</script>

<!-- Only an auth failure gets the red border. A 502 that clears itself on the next tick
     must not look like something the user has to act on. -->
<div
  class="tile {tileState}"
  class:failed={fail?.tone === "bad"}
  style="--accent-soft: {accentSoft}"
>
  <!-- The account is the least useful fact in the tile and used to eat the header, so it
       is now the name's tooltip and nothing else. -->
  <div class="head">
    <ProviderIcon id={provider.id} size={16} />
    <span class="name" title={snapshot?.account ?? undefined}>{provider.name}</span>
    {#if snapshot?.plan}
      <!-- Shrinks and ellipses; it must never wrap and push the row to two lines. -->
      <span class="chip badge" title={snapshot.plan}>{snapshot.plan}</span>
    {/if}
    <span class="gap"></span>
    {#if stale}
      <span class="stale" title="Last fetched {snapshot.fetched_at}">{stale}</span>
    {/if}
  </div>

  <!-- The raw backend string stays on the title attribute: useless on screen, useful in a
       support thread. -->
  {#if fail}
    <div class="error {fail.tone}">
      <span class="msg" title={snapshot.error}>{fail.text}</span>
      {#if fail.retry}
        <button class="btn retry" onclick={() => onRetry(provider.id)}>Retry</button>
      {/if}
    </div>
  {/if}

  <!-- Deliberately unkeyed: window labels are derived from the window length and two
       lanes of the same length share one. Row 36's keying is on provider_id, where the
       identity is real. -->
  {#each windows as w, i}
    {@const left = percentLeft(w)}
    {@const pc = pace(w, now)}
    <div class="win {step(left)}" class:hero={i === binding}>
      <div class="winhead">
        <span class="label">{w.label}</span>
        <span class="gap"></span>
        <span class="value">
          {#if left === null}
            <span class="pct">unknown</span>
          {:else}
            <span class="pct">{left}%</span><span class="unit">left</span>
          {/if}
        </span>
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
        <!-- scaleX, not width: the bar settles on the compositor and never relayouts a
             list of 23 tiles. -->
        <div class="fill" style="--v: {(left ?? 100) / 100}"></div>
      </div>
      <!-- Row 26. Absent by design under 3% elapsed, on an exhausted window, and on any
           window we have no number for: see `pace` in tiles.js. -->
      {#if pc}
        <div class="pace">
          <span class={pc.state}>{pc.label}</span>
          <!-- Null inside the pace band when the projection would argue with the label:
               see `pace` in tiles.js. -->
          {#if pc.eta}
            <span class="sep">·</span>
            <span>{pc.eta}</span>
          {/if}
        </div>
      {/if}
      {#if w.capped_by}
        <div class="capnote">Capped by {w.capped_by}</div>
      {/if}
    </div>
  {/each}

  {#if balance !== null}
    <div class="credits"><span class="k">Credits</span><b>{balance}</b></div>
  {/if}

  <!-- Out of the header, where it read as a stray border, and across the foot of the
       tile where the shape is actually a shape. -->
  <Sparkline {samples} />

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
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-raised);
    padding: var(--sp-4) var(--sp-5) var(--sp-5);
    /* One hairline of the brand color down the left edge. That is the whole accent. */
    box-shadow: inset 2px 0 0 var(--accent-soft);
  }

  /* The frame is the list-level signal. A calm or unknown tile deliberately has no
     frame of its own, which is the whole "healthy recedes" rule at tile scale. */
  .tile.watch {
    border-color: var(--state-watch-edge);
  }

  .tile.low {
    border-color: var(--state-low-edge);
  }

  .tile.spent {
    border-color: var(--state-spent-edge);
  }

  /* An auth failure outranks whatever the last good numbers said. */
  .tile.failed {
    border-color: var(--state-low-edge);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .gap {
    flex: 1 1 0;
  }

  .name {
    flex: none;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
  }

  /* Shape comes from the shared .chip. The only thing the badge adds is the right to
     shrink, so "Claude Max 20x" ellipses instead of wrapping the head onto two lines. */
  .badge {
    flex: 0 1 auto;
  }

  /* Old data is a caveat, not trouble, so it stays out of the state ramp's colours. */
  .stale {
    flex: none;
    font-size: var(--type-meta);
    color: var(--text-muted);
    white-space: nowrap;
  }

  .error {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
    margin-top: var(--sp-4);
    font-size: var(--type-body);
  }

  /* Row 21 UI half. Red is reserved for the failures that stay broken until the user
     does something; a transient blip reads like the rest of the metadata. */
  .error.bad {
    color: var(--state-low-text);
  }

  .error.warn {
    color: var(--state-watch-text);
  }

  .error.muted {
    color: var(--text-muted);
  }

  /* A transient blip gets the neutral button; only a failure the user has to act on
     borrows the ramp's red. Shape and press feedback are the shared .btn's. */
  .retry {
    --btn-fg: var(--state-low-text);
    --btn-edge: var(--state-low-edge);
    --btn-fill: none;
    flex: none;
    padding: var(--sp-1) var(--sp-3);
  }

  .error.muted .retry,
  .error.warn .retry {
    --btn-fg: var(--text-secondary);
    --btn-edge: var(--border);
  }

  /* Row 22 copy is a sentence, not a status code, so it wraps instead of ellipsing away
     the half that says what to do. */
  .msg {
    flex: 1;
    line-height: var(--leading-body);
  }

  /* --- one lane -----------------------------------------------------------------
     Every visual difference between a full lane and an exhausted one is these four
     variables plus the bar height. Nothing here knows a hex. */
  /* One step wider than the gap between a bar and its own pace line, so a lane reads as
     one group instead of the rows blurring into a wall. */
  .win {
    margin-top: var(--sp-5);
  }

  .win.calm {
    --s-bar: var(--state-calm-bar);
    --s-text: var(--state-calm-text);
    --s-weight: var(--state-calm-weight);
  }

  .win.watch {
    --s-bar: var(--state-watch-bar);
    --s-text: var(--state-watch-text);
    --s-weight: var(--state-watch-weight);
  }

  .win.low {
    --s-bar: var(--state-low-bar);
    --s-text: var(--state-low-text);
    --s-weight: var(--state-low-weight);
  }

  .win.spent {
    --s-bar: var(--state-spent-bar);
    --s-text: var(--state-spent-text);
    --s-weight: var(--state-spent-weight);
  }

  .win.unknown {
    --s-bar: var(--state-unknown-bar);
    --s-text: var(--state-unknown-text);
    --s-weight: var(--state-unknown-weight);
  }

  .winhead {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    margin-bottom: var(--sp-2);
  }

  .label {
    font-size: var(--type-meta);
    line-height: var(--leading-tight);
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
    color: var(--text-muted);
  }

  .win.hero .label {
    color: var(--text-secondary);
  }

  .value {
    display: flex;
    align-items: baseline;
    gap: var(--sp-1);
  }

  .pct {
    font-size: var(--type-body);
    line-height: var(--leading-tight);
    color: var(--s-text);
    font-weight: var(--s-weight);
  }

  /* The one number the popover exists to show. Size is the hierarchy; colour and weight
     only arrive when the ramp says there is something to worry about. */
  .win.hero .pct {
    font-size: var(--type-hero);
  }

  .unit {
    font-size: var(--type-meta);
    color: var(--text-muted);
    margin-left: 3px;
  }

  .reset {
    font-size: var(--type-meta);
    color: var(--text-muted);
    min-width: 46px;
    text-align: right;
  }

  .track {
    height: var(--bar-h-sub);
    border-radius: var(--radius-pill);
    background: var(--surface-track);
    overflow: hidden;
  }

  /* Only one lane per tile gets the thick bar, so which window binds is legible from
     across the room without reading a label. */
  .win.hero .track {
    height: var(--bar-h-hero);
  }

  /* An exhausted lane draws a zero width fill, which left the single worst state in the
     app as the one with the least ink on screen. Tinting the TRACK, not the fill, says
     "this whole window is gone" without claiming a measurement that is not there. */
  .win.spent .track {
    background: var(--state-spent-edge);
  }

  .fill {
    height: 100%;
    width: 100%;
    border-radius: inherit;
    background: var(--s-bar);
    transform: scaleX(var(--v, 1));
    transform-origin: left center;
    transition:
      transform var(--motion-slow) var(--ease),
      background-color var(--motion-fast) var(--ease);
  }

  /* Hatched, not solid: a full flat bar in any colour would read as a measurement. */
  .win.unknown .fill {
    background: repeating-linear-gradient(
      -45deg,
      var(--state-unknown-bar) 0 3px,
      transparent 3px 6px
    );
  }

  .capnote {
    margin-top: var(--sp-2);
    font-size: var(--type-meta);
    color: var(--text-muted);
  }

  /* Row 26. Subordinate to the value it qualifies: the percent is the headline, this is
     the sentence about it, so it sits a full type step below. */
  .pace {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    font-size: var(--type-meta);
    color: var(--text-muted);
  }

  /* Burning faster than the window refills is the only half of pace worth a colour.
     Being ahead is the boring case and stays silent. */
  .pace .deficit {
    color: var(--state-watch-text);
  }

  .pace .sep {
    opacity: 0.55;
  }

  .credits {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    margin-top: var(--sp-4);
  }

  .credits .k {
    font-size: var(--type-meta);
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
    color: var(--text-muted);
  }

  .credits b {
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
  }

  .tile :global(.spark) {
    margin-top: var(--sp-5);
  }

  .idle {
    margin-top: var(--sp-4);
    font-size: var(--type-body);
    line-height: var(--leading-body);
    color: var(--text-muted);
  }
</style>
