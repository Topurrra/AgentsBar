<script>
  import { onMount } from "svelte";
  import {
    call,
    openUrl,
    listBrowsers,
    setCookieSource,
    setCookieHeader,
    exportDiagnostics,
    clearCookieCache,
  } from "./api.js";
  import { providerAccent } from "./icons.js";
  import ProviderIcon from "./ProviderIcon.svelte";

  let { providers, config, onSave, onKeySaved } = $props();

  let browsers = $state([]);
  let query = $state("");
  let testing = $state(new Set());
  let testResult = $state({});

  const needsKey = (p) => p.auth === "api_key" || p.auth === "token";

  // Row 16. 23 providers is past the point where a flat alphabetical list is browsable:
  // the ones you already turned on come first, and the search field is how you find the
  // rest without scrolling past twenty cards you do not care about.
  const isOn = (p) => !!config?.providers?.[p.id]?.enabled;
  const onCount = $derived(providers.filter(isOn).length);
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const hit = (p) => `${p.name} ${p.id}`.toLowerCase().includes(q);
    return [...(q ? providers.filter(hit) : providers)].sort(
      (a, b) => Number(isOn(b)) - Number(isOn(a)),
    );
  });

  // Devin and Windsurf keep their session in the browser's localStorage (a LevelDB
  // directory), not in the cookie database, so "Auto" can never find them. They get the
  // paste field only, with the shape they actually expect.
  const MANUAL_ONLY = {
    devin: {
      placeholder: "Bearer auth1_...; https://app.devin.ai/org/<slug>",
      note: "Paste your Devin session token and your organization URL, separated by a semicolon. Stored like an API key and never shown again.",
    },
    windsurf: {
      placeholder:
        '{"devin_session_token":"...","devin_auth1_token":"...","devin_account_id":"...","devin_primary_org_id":"..."}',
      note: "Paste the four devin_* values from windsurf.com local storage as JSON. Stored like an API key and never shown again.",
    },
  };

  const entry = (id) =>
    config?.providers?.[id] ?? { enabled: false, api_key: null, cookie_source: "auto" };

  // set_cookie_source owns the enabled flag (enabled = source != "off"), so a disabled
  // provider must read as Off whatever cookie_source happens to say. Without this a fresh
  // install shows "Auto" on a provider that is off, and picking the already-selected
  // option fires no change event, so there is no way to turn it on.
  const sourceOf = (e) => (e.enabled ? (e.cookie_source ?? "auto") : "off");

  // Only the picked browser's limitation is worth a line of prose; the option label
  // already flags every limited browser.
  const browserNote = (id) => (id ? browsers.find((b) => b.id === id)?.note : null);

  onMount(async () => {
    browsers = (await listBrowsers()) ?? [];
  });

  // $state.snapshot gives a plain deep copy of the parent's config.
  function patch(mutate) {
    const next = $state.snapshot(config);
    mutate(next);
    onSave(next);
  }

  function setInterval_(e) {
    const n = Math.max(1, Math.round(Number(e.currentTarget.value) || 1));
    e.currentTarget.value = n;
    patch((c) => (c.refresh_minutes = n));
  }

  // Row 24. Adaptive is a scheduler policy, not an interval, so it is a separate flag and
  // `refresh_minutes` keeps its value underneath: turning Adaptive off returns you to the
  // interval you had, and an existing config that never saw this switch stays fixed.
  const adaptive = $derived(!!config?.refresh_adaptive);
  const setCadence = (e) => {
    const on = e.currentTarget.value === "adaptive";
    patch((c) => (c.refresh_adaptive = on));
  };

  function setStartup(e) {
    const on = e.currentTarget.checked;
    patch((c) => (c.launch_at_startup = on));
  }

  function setNotify(e) {
    const on = e.currentTarget.checked;
    patch((c) => (c.notify_on_exhaustion = on));
  }

  function setPinned(e) {
    const v = e.currentTarget.value;
    patch((c) => (c.pinned_provider = v === "" ? null : v));
  }

  // Saved on blur, like the key fields: a blank value means a direct connection. The
  // backend rebuilds the HTTP client only when this actually changes.
  function setProxy(e) {
    const v = e.currentTarget.value.trim();
    patch((c) => (c.proxy_url = v === "" ? null : v));
  }

  function setEnabled(id, e) {
    const on = e.currentTarget.checked;
    patch((c) => {
      c.providers ??= {};
      c.providers[id] = { ...(c.providers[id] ?? { api_key: null }), enabled: on };
    });
  }

  // Cookie providers go through set_cookie_source, which also owns their enabled flag,
  // so they get a source select instead of a toggle.
  async function saveSource(id, source, browser) {
    await setCookieSource(id, source, browser ?? null);
    onKeySaved();
  }

  // Row 27. The report is built and redacted in Rust. The clipboard is the whole point
  // (it ends up pasted into a GitHub issue), and the textarea is the fallback for when
  // the webview refuses clipboard access, so the user is never stuck with a report they
  // cannot get out of the app.
  let report = $state("");
  let note = $state("");

  async function exportReport() {
    note = "";
    report = "";
    const text = await exportDiagnostics();
    if (typeof text !== "string" || !text.length) {
      note = "Could not build the report.";
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      note = "Copied. Paste it into your GitHub issue.";
    } catch {
      report = text;
      note = "Clipboard blocked. Select the report below and copy it.";
    }
  }

  async function clearCookies() {
    report = "";
    note = (await clearCookieCache())
      ? "Cleared. The next refresh reads your browser again."
      : "Could not clear the cookie cache.";
    onKeySaved();
  }

  // Settings backup. Export serializes the already-redacted config (secrets never leave
  // the backend), so the JSON is safe to paste anywhere. Import parses pasted JSON and
  // hands it to set_config, which normalizes it and merges this machine's stored keys
  // back in, so a backup from another machine keeps the local credentials.
  let configNote = $state("");
  let configExport = $state("");
  let importText = $state("");
  let showImport = $state(false);

  async function exportConfig() {
    configNote = "";
    configExport = "";
    const text = JSON.stringify($state.snapshot(config), null, 2);
    try {
      await navigator.clipboard.writeText(text);
      configNote = "Copied. Paste it into a file or a message to move it.";
    } catch {
      configExport = text;
      configNote = "Clipboard blocked. Select the settings below and copy them.";
    }
  }

  function importConfig() {
    configNote = "";
    let parsed;
    try {
      parsed = JSON.parse(importText);
    } catch {
      configNote = "That is not valid JSON.";
      return;
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      configNote = "Settings must be a JSON object.";
      return;
    }
    onSave(parsed);
    importText = "";
    showImport = false;
    configNote = "Imported. Stored keys on this machine were kept.";
  }

  // Keys are saved on blur through set_api_key, never through set_config. The backend
  // never sends stored keys back, so the field starts empty and only a field the user
  // actually edited is saved. Emptying an edited field clears the stored key.
  async function saveKey(id, e) {
    const field = e.currentTarget;
    if (field.dataset.edited !== "1") return;
    field.dataset.edited = "";
    await call("set_api_key", { id, key: field.value });
    field.value = "";
    onKeySaved();
  }

  // Same discipline for the pasted Cookie header: never prefilled, cleared right after
  // the backend takes it, so the secret is in the DOM only while it is being typed.
  async function saveHeader(id, e) {
    const field = e.currentTarget;
    if (field.dataset.edited !== "1") return;
    field.dataset.edited = "";
    await setCookieHeader(id, field.value);
    field.value = "";
    onKeySaved();
  }

  async function testProvider(id) {
    testing.update((s) => new Set([...s, id]));
    testResult[id] = null;
    await call("refresh_provider", { id });
    const snapshots = await call("get_snapshots");
    const snap = snapshots?.find((s) => s.provider_id === id);
    testResult[id] = snap?.error ? { ok: false } : { ok: true };
    testing.update((s) => {
      const next = new Set([...s]);
      next.delete(id);
      return next;
    });
    setTimeout(() => {
      testResult[id] = null;
    }, 4000);
  }
</script>

<div class="scroll">
  <div class="secthead">
    <h2>General</h2>
  </div>

  <section>
    <div class="field">
      <div class="row">
        <label for="cadence">Refresh</label>
        <span class="gap"></span>
        <span class="sel cadence">
          <select id="cadence" value={adaptive ? "adaptive" : "fixed"} onchange={setCadence}>
            <option value="adaptive">Adaptive</option>
            <option value="fixed">Every</option>
          </select>
        </span>
        {#if !adaptive}
          <input
            id="ivl"
            type="number"
            min="1"
            step="1"
            class="num"
            aria-label="Refresh interval in minutes"
            value={config?.refresh_minutes ?? 5}
            onchange={setInterval_}
          />
          <span class="unit">min</span>
        {/if}
      </div>
      {#if adaptive}
        <p class="note">
          Checks often just after you open AgentsBar and backs off to 30 minutes while you
          are away, or on battery saver.
        </p>
      {/if}
    </div>

    <div class="field">
      <div class="row">
        <label for="startup">Launch at startup</label>
        <span class="gap"></span>
        <span class="switch">
          <input
            id="startup"
            type="checkbox"
            checked={!!config?.launch_at_startup}
            onchange={setStartup}
          />
          <span></span>
        </span>
      </div>
    </div>

    <div class="field">
      <div class="row">
        <label for="notify">Notify when a provider runs out</label>
        <span class="gap"></span>
        <span class="switch">
          <input
            id="notify"
            type="checkbox"
            checked={config?.notify_on_exhaustion ?? true}
            onchange={setNotify}
          />
          <span></span>
        </span>
      </div>
    </div>

    <div class="field">
      <div class="row">
        <label for="pinned">Tray provider</label>
        <span class="gap"></span>
        <span class="sel">
          <select id="pinned" value={config?.pinned_provider ?? ""} onchange={setPinned}>
            <option value="">Auto</option>
            {#each providers as p (p.id)}
              <option value={p.id}>{p.name}</option>
            {/each}
          </select>
        </span>
      </div>
    </div>

    <div class="field">
      <div class="row">
        <label for="proxy">Proxy</label>
      </div>
      <input
        id="proxy"
        type="text"
        class="proxy"
        placeholder="http://proxy:8080"
        autocomplete="off"
        spellcheck="false"
        value={config?.proxy_url ?? ""}
        onblur={setProxy}
      />
      <p class="note">
        Routes all provider requests through this proxy. Leave blank for a direct connection.
      </p>
    </div>
  </section>

  <div class="secthead">
    <h2>Support</h2>
  </div>

  <section>
    <div class="field">
      <div class="row wrap">
        <label for="diag">Diagnostics report</label>
        <span class="gap"></span>
        <span class="btns">
          <button id="diag" class="btn" onclick={exportReport}>Copy report</button>
          <button class="btn" onclick={clearCookies}>Clear cached cookies</button>
        </span>
      </div>
      {#if note}
        <p class="note">{note}</p>
      {/if}
      {#if report}
        <textarea class="report" readonly rows="6" spellcheck="false" value={report}></textarea>
      {/if}
    </div>

    <div class="field">
      <div class="row wrap">
        <label for="exportcfg">Settings backup</label>
        <span class="gap"></span>
        <span class="btns">
          <button id="exportcfg" class="btn" onclick={exportConfig}>Copy settings</button>
          <button class="btn" onclick={() => (showImport = !showImport)}>
            {showImport ? "Hide import" : "Import settings"}
          </button>
        </span>
      </div>
      {#if configNote}
        <p class="note">{configNote}</p>
      {/if}
      {#if configExport}
        <textarea class="report" readonly rows="6" spellcheck="false" value={configExport}></textarea>
      {/if}
      {#if showImport}
        <textarea
          class="report"
          rows="6"
          spellcheck="false"
          bind:value={importText}
          placeholder="Paste settings JSON here…"
        ></textarea>
        <div class="row">
          <span class="gap"></span>
          <button class="btn" onclick={importConfig}>Apply</button>
        </div>
      {/if}
    </div>
  </section>

  <div class="secthead">
    <h2>Providers</h2>
    <span class="chip count">{onCount} on</span>
    <span class="gap"></span>
    <span class="searchwrap">
      <svg
        class="searchico"
        width="11"
        height="11"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="1.7"
        aria-hidden="true"
      >
        <circle cx="6.8" cy="6.8" r="4.6" />
        <path d="M10.3 10.3 14 14" stroke-linecap="round" />
      </svg>
      <input
        class="search"
        type="search"
        placeholder="Search"
        aria-label="Search providers"
        autocomplete="off"
        spellcheck="false"
        bind:value={query}
      />
    </span>
  </div>

  {#if !providers.length}
    <p class="hint">No providers registered.</p>
  {:else if !shown.length}
    <p class="hint">No provider matches "{query}".</p>
  {/if}

  {#each shown as p (p.id)}
    {@const e = entry(p.id)}
    {@const cookie = p.auth === "cookie"}
    {@const manual = MANUAL_ONLY[p.id]}
    {@const stored = sourceOf(e)}
    {@const src = manual && stored !== "off" ? "manual" : stored}
    <div
      class="prov"
      class:on={e.enabled}
      style="--accent-soft: {providerAccent(p.id)}40"
    >
      <div class="row">
        <span class="ico"><ProviderIcon id={p.id} size={15} /></span>
        <span class="name">{p.name}</span>
        {#if cookie}
          <span class="chip kind">session</span>
        {/if}
        {#if e.enabled && !p.configured}
          <span class="chip tag">needs auth</span>
        {/if}
        <span class="gap"></span>
        {#if p.doc_url}
          <button class="btn docs" onclick={() => openUrl(p.doc_url)}>Docs</button>
        {/if}
        {#if e.enabled && p.configured}
          {@const isTesting = testing.has(p.id)}
          {@const result = testResult[p.id]}
          <button
            class="btn docs"
            class:testing={isTesting}
            class:ok={result?.ok === true}
            class:fail={result?.ok === false}
            disabled={isTesting}
            onclick={() => testProvider(p.id)}
          >
            {isTesting ? "Testing\u2026" : result?.ok === true ? "OK" : result?.ok === false ? "Failed" : "Test"}
          </button>
        {/if}
        {#if cookie}
          <span class="sel src">
            <select
              aria-label={"Cookie source for " + p.name}
              value={src}
              onchange={(ev) => saveSource(p.id, ev.currentTarget.value, e.cookie_browser)}
            >
              {#if !manual}<option value="auto">Auto</option>{/if}
              <option value="manual">Manual</option>
              <option value="off">Off</option>
            </select>
          </span>
        {:else}
          <span class="switch">
            <input
              type="checkbox"
              aria-label={"Enable " + p.name}
              checked={e.enabled}
              onchange={(ev) => setEnabled(p.id, ev)}
            />
            <span></span>
          </span>
        {/if}
      </div>

      {#if cookie && src === "auto"}
        <div class="sub">
          <div class="row">
            <label for={"br-" + p.id} class="sublabel">Browser</label>
            <span class="gap"></span>
            <span class="sel">
              <select
                id={"br-" + p.id}
                value={e.cookie_browser ?? ""}
                onchange={(ev) => saveSource(p.id, "auto", ev.currentTarget.value)}
              >
                <option value="">Any detected</option>
                {#each browsers as b (b.id)}
                  <option value={b.id}>{b.label}{b.supported ? "" : " (limited)"}</option>
                {/each}
              </select>
            </span>
          </div>
          {#if !browsers.length}
            <p class="note">
              No browser cookie database found. Sign in with Chrome, Edge or Firefox, or
              switch this provider to Manual.
            </p>
          {:else if browserNote(e.cookie_browser)}
            <p class="note">{browserNote(e.cookie_browser)}</p>
          {/if}
        </div>
      {:else if cookie && src === "manual"}
        <div class="sub">
          <span class="secret" class:saved={p.configured}>
            <svg
              class="lockico"
              width="11"
              height="11"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              aria-hidden="true"
            >
              <rect x="3.2" y="7" width="9.6" height="6.6" rx="1.6" />
              <path d="M5.6 7V5.2a2.4 2.4 0 0 1 4.8 0V7" stroke-linecap="round" />
            </svg>
            <textarea
              class="key"
              rows="2"
              placeholder={p.configured
                ? "Saved, paste to replace"
                : (manual?.placeholder ?? "name=value; name2=value2")}
              autocomplete="off"
              spellcheck="false"
              oninput={(ev) => (ev.currentTarget.dataset.edited = "1")}
              onblur={(ev) => saveHeader(p.id, ev)}
            ></textarea>
          </span>
          <p class="note">
            {manual?.note ??
              "Paste the Cookie header from your signed in browser. Stored like an API key and never shown again."}
          </p>
        </div>
      {:else if needsKey(p)}
        <span class="secret" class:saved={p.configured}>
          <svg
            class="lockico"
            width="11"
            height="11"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            aria-hidden="true"
          >
            <rect x="3.2" y="7" width="9.6" height="6.6" rx="1.6" />
            <path d="M5.6 7V5.2a2.4 2.4 0 0 1 4.8 0V7" stroke-linecap="round" />
          </svg>
          <input
            type="password"
            class="key"
            placeholder={p.configured
              ? "Saved, type to replace"
              : p.auth === "token"
                ? "Token"
                : "API key"}
            autocomplete="off"
            spellcheck="false"
            value=""
            oninput={(ev) => (ev.currentTarget.dataset.edited = "1")}
            onblur={(ev) => saveKey(p.id, ev)}
          />
        </span>
      {/if}
    </div>
  {/each}
</div>

<style>
  /* Same 12px card inset as the tile list: the right value is one step down because
     .scroll reserves a stable 8px scrollbar gutter on top of it. */
  .scroll {
    padding: var(--sp-1) var(--sp-2) var(--sp-6) var(--sp-5);
  }

  /* --- Section rhythm -----------------------------------------------------
     A heading sits outside its card, so the eye gets label, group, label,
     group instead of one undifferentiated scroll of boxes. */
  .secthead {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin: var(--sp-6) 0 var(--sp-3);
  }

  .secthead:first-child {
    margin-top: var(--sp-4);
  }

  h2 {
    margin: 0;
    font-size: var(--type-meta);
    font-weight: var(--weight-medium);
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
    color: var(--text-secondary);
  }

  .count {
    color: var(--text-muted);
  }

  /* --- Grouped card -------------------------------------------------------
     One card per section, hairline between rows. A divider that only spans
     the text column would ripple; these run the full width of the card. */
  section {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-raised);
    overflow: hidden;
  }

  .field {
    padding: 0 var(--sp-5);
  }

  .field + .field {
    border-top: 1px solid var(--border);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    min-height: 36px;
  }

  /* Two buttons plus a label do not fit on 380px, so the pair drops to its own
     line as a unit and stays right aligned instead of breaking apart. */
  .row.wrap {
    flex-wrap: wrap;
    justify-content: flex-end;
    row-gap: var(--sp-3);
    padding: var(--sp-3) 0;
  }

  .btns {
    display: flex;
    flex: none;
    gap: var(--sp-3);
  }

  .gap {
    flex: 1;
  }

  label {
    color: var(--text-primary);
    font-weight: var(--weight-medium);
  }

  .num {
    width: 52px;
    text-align: right;
  }

  .unit {
    color: var(--text-muted);
    font-size: var(--type-meta);
    width: 18px;
  }

  /* --- Controls -----------------------------------------------------------
     The native select arrow is the one piece of browser chrome that reads as
     unstyled on a dark surface, so it is replaced by a chevron drawn from two
     borders in currentColor: no image, no icon font, nothing to load. */
  .sel {
    position: relative;
    display: inline-flex;
    flex: none;
  }

  .sel select {
    appearance: none;
    max-width: 150px;
    padding-right: 22px;
    cursor: pointer;
  }

  .sel::after {
    content: "";
    position: absolute;
    right: 9px;
    top: 50%;
    width: 5px;
    height: 5px;
    border-right: 1.5px solid var(--text-muted);
    border-bottom: 1.5px solid var(--text-muted);
    transform: translateY(-70%) rotate(45deg);
    pointer-events: none;
  }

  .sel:hover::after {
    border-color: var(--text-secondary);
  }

  .cadence select {
    max-width: 96px;
  }

  .src select {
    padding: var(--sp-1) 20px var(--sp-1) var(--sp-3);
  }

  .src::after {
    right: 7px;
  }

  /* Shape, hover and press come from the shared .btn in app.css. Docs is the quiet
     variant: no fill, tighter box, same radius as every other button. */
  .docs {
    --btn-fill: none;
    padding: var(--sp-1) var(--sp-3);
  }

  .docs.ok {
    --btn-fg: var(--state-calm-text);
    --btn-edge: var(--state-calm-bar);
  }

  .docs.fail {
    --btn-fg: var(--state-low-text);
    --btn-edge: var(--state-low-edge);
  }

  .docs:disabled {
    opacity: 0.6;
  }

  /* --- Provider card ------------------------------------------------------
     Enabled and disabled must be legible at a glance across 23 rows: an on
     provider is a filled card with its brand rule and a full contrast name,
     an off one is an outline with a desaturated mark. Nothing animates. */
  .prov {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: transparent;
    padding: 0 var(--sp-5) var(--sp-3);
    margin-bottom: var(--sp-3);
    overflow: hidden;
    transition: border-color var(--motion-fast) var(--ease);
  }

  .prov.on {
    background: var(--surface-raised);
    box-shadow: inset 2px 0 0 var(--accent-soft);
  }

  .prov:hover {
    border-color: var(--border-strong);
  }

  .prov .row {
    min-height: 34px;
    gap: var(--sp-3);
  }

  .ico {
    display: flex;
    flex: none;
    filter: grayscale(1);
    opacity: 0.45;
  }

  .prov.on .ico {
    filter: none;
    opacity: 1;
  }

  .name {
    color: var(--text-secondary);
    font-weight: var(--weight-regular);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .prov.on .name {
    color: var(--text-primary);
    font-weight: var(--weight-medium);
  }

  /* Cookie providers get an indented block instead of a bare field, so the browser
     picker reads as belonging to that provider. The negative margin lets the rule
     span the whole card. */
  .sub {
    border-top: 1px solid var(--border);
    margin: 0 calc(-1 * var(--sp-5));
    padding: 0 var(--sp-5) var(--sp-2);
  }

  .sublabel {
    color: var(--text-secondary);
    font-weight: var(--weight-regular);
  }

  /* Both are the shared .chip. "session" is a category label so it takes the same
     uppercase micro treatment as a window label in a tile; "needs auth" is the one
     chip that has to warn, and it swaps colour without changing shape. */
  .kind,
  .tag {
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
  }

  .kind {
    color: var(--text-muted);
  }

  /* Same fill as every other chip; only the ink changes. Tinting the fill too would
     drop the amber on amber contrast to 4.2:1, and colour alone already reads. */
  .tag {
    color: var(--state-watch-text);
  }

  /* --- Secret fields ------------------------------------------------------
     The stored value is never sent back to the UI, so the field is always
     empty; the lock plus the placeholder are the only thing telling the user
     whether a key exists. A saved field says so at readable contrast, an
     empty one stays muted like the prompt it is. */
  .secret {
    position: relative;
    display: block;
    margin-top: var(--sp-2);
  }

  .lockico {
    position: absolute;
    left: var(--sp-4);
    top: var(--sp-4);
    color: var(--text-muted);
    pointer-events: none;
  }

  .secret.saved .lockico {
    color: var(--text-secondary);
  }

  .key {
    width: 100%;
    padding-left: 26px;
    letter-spacing: 0.5px;
  }

  .proxy {
    width: 100%;
    margin-top: var(--sp-2);
  }

  .key::placeholder {
    color: var(--text-muted);
    letter-spacing: normal;
  }

  .secret.saved .key::placeholder {
    color: var(--text-secondary);
  }

  textarea.key {
    resize: none;
    line-height: var(--leading-body);
  }

  .note {
    margin: 0 0 var(--sp-3);
    font-size: var(--type-meta);
    line-height: var(--leading-body);
    color: var(--text-muted);
  }

  .searchwrap {
    position: relative;
    display: inline-flex;
    align-items: center;
  }

  .searchico {
    position: absolute;
    left: var(--sp-3);
    color: var(--text-muted);
    pointer-events: none;
  }

  .search {
    width: 132px;
    padding: var(--sp-2) var(--sp-3) var(--sp-2) 22px;
  }

  .search::-webkit-search-cancel-button {
    cursor: pointer;
  }

  /* The report is the one place in the app where text is meant to be selected: the whole
     point is getting it into an issue. */
  .report {
    width: 100%;
    margin: 0 0 var(--sp-4);
    font-family: ui-monospace, Consolas, monospace;
    font-size: var(--type-meta);
    line-height: var(--leading-body);
    resize: none;
    user-select: text;
  }

  .hint {
    margin: 0;
    padding: var(--sp-7) 0;
    text-align: center;
    color: var(--text-muted);
  }
</style>
