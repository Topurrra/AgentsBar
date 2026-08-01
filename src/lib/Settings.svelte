<script>
  import { onMount } from "svelte";
  import {
    call,
    openUrl,
    listBrowsers,
    setCookieSource,
    setCookieHeader,
  } from "./api.js";
  import { providerAccent } from "./icons.js";
  import ProviderIcon from "./ProviderIcon.svelte";

  let { providers, config, onSave, onKeySaved } = $props();

  let browsers = $state([]);

  const needsKey = (p) => p.auth === "api_key" || p.auth === "token";

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

  function setStartup(e) {
    const on = e.currentTarget.checked;
    patch((c) => (c.launch_at_startup = on));
  }

  function setPinned(e) {
    const v = e.currentTarget.value;
    patch((c) => (c.pinned_provider = v === "" ? null : v));
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
</script>

<div class="scroll">
  <section>
    <div class="row">
      <label for="ivl">Refresh interval</label>
      <span class="gap"></span>
      <input
        id="ivl"
        type="number"
        min="1"
        step="1"
        class="num"
        value={config?.refresh_minutes ?? 5}
        onchange={setInterval_}
      />
      <span class="unit">min</span>
    </div>

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

    <div class="row">
      <label for="pinned">Tray provider</label>
      <span class="gap"></span>
      <select id="pinned" value={config?.pinned_provider ?? ""} onchange={setPinned}>
        <option value="">Auto</option>
        {#each providers as p (p.id)}
          <option value={p.id}>{p.name}</option>
        {/each}
      </select>
    </div>
  </section>

  <h2>Providers</h2>

  {#if !providers.length}
    <p class="hint">No providers registered.</p>
  {/if}

  {#each providers as p (p.id)}
    {@const e = entry(p.id)}
    {@const cookie = p.auth === "cookie"}
    {@const manual = MANUAL_ONLY[p.id]}
    {@const stored = sourceOf(e)}
    {@const src = manual && stored !== "off" ? "manual" : stored}
    <div class="prov" style="--accent-soft: {providerAccent(p.id)}40">
      <div class="row">
        <ProviderIcon id={p.id} size={15} />
        <span class="name">{p.name}</span>
        {#if cookie}
          <span class="chip">session</span>
        {/if}
        {#if e.enabled && !p.configured}
          <span class="tag">needs auth</span>
        {/if}
        <span class="gap"></span>
        {#if p.doc_url}
          <button class="docs" onclick={() => openUrl(p.doc_url)}>Docs</button>
        {/if}
        {#if cookie}
          <select
            class="src"
            aria-label={"Cookie source for " + p.name}
            value={src}
            onchange={(ev) => saveSource(p.id, ev.currentTarget.value, e.cookie_browser)}
          >
            {#if !manual}<option value="auto">Auto</option>{/if}
            <option value="manual">Manual</option>
            <option value="off">Off</option>
          </select>
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
          <p class="note">
            {manual?.note ??
              "Paste the Cookie header from your signed in browser. Stored like an API key and never shown again."}
          </p>
        </div>
      {:else if needsKey(p)}
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
      {/if}
    </div>
  {/each}
</div>

<style>
  .scroll {
    padding: 10px;
  }

  section {
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--panel);
    padding: 4px 10px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 34px;
  }

  .gap {
    flex: 1;
  }

  label,
  .name {
    color: var(--text);
  }

  .name {
    font-weight: 600;
  }

  .num {
    width: 54px;
    text-align: right;
  }

  .unit {
    color: var(--faint);
    font-size: 11px;
    width: 20px;
  }

  select {
    max-width: 150px;
  }

  .src {
    font-size: 12px;
    padding: 3px 6px;
  }

  h2 {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.6px;
    color: var(--faint);
    font-weight: 600;
    margin: 16px 0 6px 2px;
  }

  .prov {
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--panel);
    padding: 2px 10px 8px 11px;
    margin-bottom: 6px;
    box-shadow: inset 2px 0 0 var(--accent-soft);
  }

  .prov .row {
    min-height: 32px;
  }

  /* Cookie providers get an indented block instead of a bare field, so the browser
     picker reads as belonging to that provider. */
  .sub {
    border-top: 1px solid var(--line);
    margin-top: 2px;
    padding-top: 2px;
  }

  .sublabel {
    color: var(--dim);
    font-size: 12px;
  }

  .chip {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 1px 5px;
  }

  .tag {
    font-size: 10px;
    color: var(--warn);
    border: 1px solid rgba(214, 160, 40, 0.35);
    border-radius: 4px;
    padding: 1px 5px;
  }

  .docs {
    font-size: 11px;
    color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 5px;
    padding: 1px 7px;
    transition: background 0.12s ease, color 0.12s ease;
  }

  .docs:hover {
    background: rgba(255, 255, 255, 0.07);
    color: var(--text);
  }

  .key {
    width: 100%;
    margin-top: 2px;
    font-size: 12px;
    letter-spacing: 0.5px;
  }

  textarea.key {
    resize: none;
    line-height: 1.4;
  }

  .note {
    margin: 5px 0 2px;
    font-size: 11px;
    line-height: 1.45;
    color: var(--faint);
  }

  .hint {
    margin: 0 0 8px 2px;
    color: var(--faint);
  }
</style>
