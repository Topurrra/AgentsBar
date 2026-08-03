<script>
  // The desktop widget: a small always-on-top glance at each active provider's limits.
  // Same data as the popover (one process, so it rides the same commands and
  // `usage-updated` event; no snapshot file like CodexBar needs for its separate-process
  // WidgetKit extension).
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { call } from "./api.js";
  import {
    costSummary,
    recommend,
    reorderProviderOrder,
    sortTiles,
    widgetDisplay,
    oldestFetch,
    ago,
  } from "./tiles.js";
  import { usd, credits, percentLeft, tone } from "./format.js";
  import { applyTheme, systemLight } from "./theme.js";
  import ProviderIcon from "./ProviderIcon.svelte";

  let providers = $state([]);
  let snapshots = $state([]);
  let config = $state(null);
  let now = $state(Date.now());
  let hiding = $state(false);
  let savingOrder = $state(false);
  let draggingProvider = $state(null);
  let dropProvider = $state(null);
  let dropAfter = $state(false);

  const byId = $derived(new Map(snapshots.map((s) => [s.provider_id, s])));
  const enabled = $derived(
    providers.filter((p) => config?.providers?.[p.id]?.enabled),
  );
  const shown = $derived(enabled.map((p) => byId.get(p.id)).filter(Boolean));

  const rec = $derived(enabled.length >= 2 ? recommend(enabled, byId) : null);

  // Every enabled provider stays visible. A compact row can show quota windows, a
  // balance, an actionable problem, or that it is waiting for its first refresh.
  const rows = $derived(sortTiles(enabled, byId, config?.provider_order ?? []));

  const cost = $derived(costSummary(shown));
  const costLabel = $derived.by(() => {
    if (!cost) return "";
    const parts = [];
    if (cost.hasSpend) parts.push(`${usd(cost.spend)} spent`);
    if (cost.hasBalance) parts.push(`${usd(cost.balance)} balance`);
    return parts.join(" · ");
  });

  const refreshed = $derived(ago(oldestFetch(shown), now));

  const step = (left) =>
    left === null ? "unknown" : left === 0 ? "spent" : { ok: "calm", warn: "watch", bad: "low" }[tone(left)];

  // `data-tauri-drag-region` does not take on this transparent window, so drag the
  // header explicitly. The scrollable provider area and all controls remain normal UI.
  function startDrag(e) {
    const target = e.target instanceof Element ? e.target : null;
    if (e.button !== 0 || target?.closest("button")) return;
    getCurrentWindow().startDragging().catch((err) => console.error("startDragging", err));
  }

  async function hide() {
    // The widget is only ever visible when this button is on screen, so toggling hides it.
    if (hiding) return;
    hiding = true;
    try {
      await call("toggle_widget");
    } catch (err) {
      console.error("toggle_widget", err);
    } finally {
      hiding = false;
    }
  }

  async function saveProviderOrder(order) {
    if (!config || savingOrder) return;
    const previous = config.provider_order ?? [];
    savingOrder = true;
    config = { ...config, provider_order: order };
    try {
      const saved = await call("set_provider_order", { order });
      if (Array.isArray(saved)) config = { ...config, provider_order: saved };
      else config = { ...config, provider_order: previous };
    } catch (err) {
      console.error("set_provider_order", err);
      config = { ...config, provider_order: previous };
    } finally {
      savingOrder = false;
    }
  }

  function moveProvider(id, targetId, after = false) {
    if (id === targetId) return;
    const order = sortTiles(providers, byId, config?.provider_order ?? []).map((p) => p.id);
    saveProviderOrder(reorderProviderOrder(order, id, targetId, after));
  }

  // Pointer capture works in the desktop webview while HTML drag-and-drop does not
  // reliably begin there. The header keeps its separate native window-drag behavior.
  function startProviderReorder(e, id) {
    if (savingOrder || e.button !== 0) return;
    e.preventDefault();
    draggingProvider = id;
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  function updateProviderDropTarget(e) {
    const target = document.elementFromPoint(e.clientX, e.clientY)?.closest("[data-provider-id]");
    const id = target?.dataset.providerId;
    if (!draggingProvider || !id || id === draggingProvider) {
      dropProvider = null;
      return;
    }
    const bounds = target.getBoundingClientRect();
    dropProvider = id;
    dropAfter = e.clientY > bounds.top + bounds.height / 2;
  }

  function finishProviderDrag() {
    draggingProvider = null;
    dropProvider = null;
    dropAfter = false;
  }

  function finishProviderReorder(e) {
    updateProviderDropTarget(e);
    if (draggingProvider && dropProvider) moveProvider(draggingProvider, dropProvider, dropAfter);
    finishProviderDrag();
  }

  function moveProviderWithKeys(e, id) {
    if (!e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
    const index = rows.findIndex((p) => p.id === id);
    const target = rows[index + (e.key === "ArrowUp" ? -1 : 1)];
    if (!target) return;
    e.preventDefault();
    moveProvider(id, target.id, e.key === "ArrowDown");
  }

  async function load() {
    const [cfg, list, snaps] = await Promise.all([
      call("get_config"),
      call("list_providers"),
      call("get_snapshots"),
    ]);
    if (cfg) config = cfg;
    if (list) providers = list;
    if (snaps) snapshots = snaps;
  }

  const creditLabel = (display) =>
    display.unit === "USD"
      ? usd(display.value)
      : `${credits(display.value)}${display.unit ? ` ${display.unit}` : " credits"}`;

  $effect(() => applyTheme(config?.theme ?? "auto"));

  onMount(() => {
    load();
    const tick = setInterval(() => (now = Date.now()), 30000);
    const sub = listen("usage-updated", () => load());
    // Emitted by the backend whenever the widget is shown: pull fresh numbers in case
    // this webview loaded before the first refresh had anything.
    const subShown = listen("widget-shown", () => load());
    const subOrder = listen("provider-order-updated", (e) => {
      if (Array.isArray(e.payload) && config) {
        config = { ...config, provider_order: e.payload };
      } else {
        load();
      }
    });
    const onScheme = () => applyTheme(config?.theme ?? "auto");
    systemLight.addEventListener("change", onScheme);
    return () => {
      clearInterval(tick);
      systemLight.removeEventListener("change", onScheme);
      sub.then((un) => un()).catch(() => {});
      subShown.then((un) => un()).catch(() => {});
      subOrder.then((un) => un()).catch(() => {});
    };
  });
</script>

<div class="widget">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <header onmousedown={startDrag}>
    <span class="mark" aria-hidden="true">
      <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
        <rect x="1.5" y="9" width="3" height="5" rx="1.5" opacity="0.34" />
        <rect x="6.5" y="5.5" width="3" height="8.5" rx="1.5" opacity="0.62" />
        <rect x="11.5" y="2" width="3" height="12" rx="1.5" />
      </svg>
    </span>
    <span class="title">AgentsBar</span>
    {#if refreshed}
      <span class="age" title="Oldest snapshot on screen">{refreshed}</span>
    {/if}
    <span class="gap"></span>
    <button class="hide" title="Hide widget" aria-label="Hide widget" onclick={hide} disabled={hiding}>
      <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.6">
        <path d="M2 2l8 8M10 2l-8 8" stroke-linecap="round" />
      </svg>
    </button>
  </header>

  {#if !rows.length}
    <div class="empty">No providers enabled. Turn one on in the popover.</div>
  {:else}
    <div class="body">
      {#each rows as p (p.id)}
        {@const snap = byId.get(p.id)}
        {@const display = widgetDisplay(p, snap, config?.providers?.[p.id]?.cookie_source)}
        {@const isRec = rec?.provider.id === p.id}
        <div
          class="prov"
          data-provider-id={p.id}
          class:rec={isRec}
          class:dragging={draggingProvider === p.id}
          class:drop-target={dropProvider === p.id && draggingProvider !== p.id}
          class:drop-after={dropProvider === p.id && dropAfter}
        >
          <div class="prov-head">
            <ProviderIcon id={p.id} size={13} />
            <span class="identity">
              <span class="prov-name">{p.name}</span>
              {#if isRec}
                <span class="rec-tag" title="Most room. Reach for this one first">most room</span>
              {/if}
            </span>
            <button
              class="reorder"
              type="button"
              disabled={savingOrder}
              aria-label={`Reorder ${p.name}. Use Alt+Up or Alt+Down to move it.`}
              aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
              title="Drag to reorder. Alt+Up/Down moves it."
              onpointerdown={(e) => startProviderReorder(e, p.id)}
              onpointermove={updateProviderDropTarget}
              onpointerup={finishProviderReorder}
              onpointercancel={finishProviderDrag}
              onkeydown={(e) => moveProviderWithKeys(e, p.id)}
            >
              <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor" aria-hidden="true">
                <circle cx="3" cy="2.5" r="1" /><circle cx="9" cy="2.5" r="1" />
                <circle cx="3" cy="6" r="1" /><circle cx="9" cy="6" r="1" />
                <circle cx="3" cy="9.5" r="1" /><circle cx="9" cy="9.5" r="1" />
              </svg>
            </button>
          </div>
          {#if display.kind === "windows"}
            {#each display.windows as w}
              {@const left = percentLeft(w)}
              {@const st = step(left)}
              <div class="lane">
                <span class="lane-label">{w.label}</span>
                <span class="bar {st}" aria-hidden="true">
                  <span class="fill" style="--v: {(left ?? 100) / 100}"></span>
                </span>
                <span class="lane-pct">{left === null ? "?" : left + "%"}</span>
              </div>
            {/each}
          {:else if display.kind === "credits"}
            <div class="detail"><span>Balance</span><b>{creditLabel(display)}</b></div>
          {:else if display.kind === "spend"}
            <div class="detail"><span>Spend</span><b>{usd(display.value)}</b></div>
          {:else if display.kind === "error"}
            <div class="detail error {display.tone}">{display.text}</div>
          {:else if display.kind === "setup"}
            <div class="detail">Needs setup in the popover</div>
          {:else}
            <div class="detail">Waiting for first update</div>
          {/if}
        </div>
      {/each}
    </div>

    {#if costLabel}
      <footer>{costLabel}</footer>
    {/if}
  {/if}
</div>

<style>
  .widget {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: linear-gradient(
      160deg,
      var(--surface-float-raised) 0%,
      var(--surface-float) 48%,
      var(--surface-float-raised) 100%
    );
    color: var(--text-primary);
    overflow: hidden;
    /* The window is transparent, so these rounded corners are what you actually see on
       both Windows 10 and 11, with a hairline border for definition against the desktop. */
    border-radius: var(--radius-lg);
    border: 1px solid var(--border-strong);
    box-shadow: inset 0 1px 0 var(--sheen);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex: none;
    padding: var(--sp-3) var(--sp-3) var(--sp-2) var(--sp-4);
    cursor: grab;
    user-select: none;
  }

  header:active {
    cursor: grabbing;
  }

  .title {
    font-size: var(--type-body);
    font-weight: var(--weight-bold);
    letter-spacing: 0.2px;
  }

  .mark {
    display: flex;
    flex: none;
    color: var(--text-primary);
    pointer-events: none;
  }

  .gap {
    flex: 1 1 0;
  }

  .age {
    font-size: var(--type-meta);
    color: var(--text-muted);
    white-space: nowrap;
  }

  .hide {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    transition:
      background var(--motion-fast) var(--ease),
      color var(--motion-fast) var(--ease);
  }

  .hide:hover {
    background: var(--overlay-hover);
    color: var(--text-primary);
  }

  .hide svg {
    width: 14px;
    height: 14px;
  }

  .empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--sp-5);
    text-align: center;
    font-size: var(--type-body);
    color: var(--text-muted);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: 0 var(--sp-3) var(--sp-3) var(--sp-4);
    overflow-y: auto;
    overscroll-behavior: contain;
  }

  /* One provider: a name head and one lane per quota window (5h, weekly, …). The
     recommended provider gets the calm hairline used by the matching popover row. */
  .prov {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-md);
    background: var(--surface-float-raised);
    border: 1px solid var(--border);
  }

  .prov.rec {
    border-color: var(--state-calm-bar);
    box-shadow: inset 2px 0 0 var(--state-calm-bar);
  }

  .prov.dragging {
    opacity: 0.55;
  }

  .prov.drop-target {
    border-color: var(--state-calm-bar);
  }

  .prov.drop-target.drop-after {
    box-shadow: inset 0 -2px 0 var(--state-calm-bar);
  }

  .prov.drop-target:not(.drop-after) {
    box-shadow: inset 0 2px 0 var(--state-calm-bar);
  }

  .prov-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .identity {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }

  .prov-name {
    flex: 0 1 auto;
    min-width: 0;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rec-tag {
    flex: none;
    font-size: var(--type-meta);
    font-weight: var(--weight-medium);
    color: var(--state-calm-text);
    border: 1px solid var(--state-calm-bar);
    border-radius: var(--radius-sm);
    padding: 0 var(--sp-2);
    line-height: 1.5;
  }

  .reorder {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    margin: -4px -4px -4px 0;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    cursor: grab;
    touch-action: none;
  }

  .reorder:hover {
    background: var(--overlay-hover);
    color: var(--text-primary);
  }

  .reorder:active {
    cursor: grabbing;
  }

  .reorder:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .lane {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .lane-label {
    flex: none;
    width: 52px;
    font-size: var(--type-meta);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bar {
    flex: 1 1 0;
    min-width: 0;
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
  }

  .bar.calm {
    color: var(--state-calm-bar);
  }
  .bar.watch {
    color: var(--state-watch-bar);
  }
  .bar.low {
    color: var(--state-low-bar);
  }
  .bar.spent {
    color: var(--state-spent-bar);
  }
  .bar.unknown {
    color: var(--surface-hatch);
  }

  .lane-pct {
    flex: none;
    width: 36px;
    text-align: right;
    font-size: var(--type-meta);
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
  }

  .detail {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    min-width: 0;
    color: var(--text-muted);
    font-size: var(--type-meta);
  }

  .detail b {
    color: var(--text-primary);
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
  }

  .detail.error {
    display: block;
    line-height: var(--leading-body);
  }

  .detail.error.bad {
    color: var(--state-low-text);
  }

  .detail.error.warn {
    color: var(--state-watch-text);
  }

  .detail.error.muted {
    color: var(--text-secondary);
  }

  footer {
    flex: none;
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
    border-top: 1px solid var(--border);
    font-size: var(--type-meta);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
