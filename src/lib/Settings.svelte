<script>
  import { call, openUrl } from "./api.js";

  let { providers, config, onSave, onKeySaved } = $props();

  const needsKey = (p) => p.auth === "api_key" || p.auth === "token";
  const entry = (id) => config?.providers?.[id] ?? { enabled: false, api_key: null };

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
    <div class="prov">
      <div class="row">
        <span class="name">{p.name}</span>
        {#if entry(p.id).enabled && !p.configured}
          <span class="tag">needs auth</span>
        {/if}
        <span class="gap"></span>
        {#if p.doc_url}
          <button class="docs" onclick={() => openUrl(p.doc_url)}>Docs</button>
        {/if}
        <span class="switch">
          <input
            type="checkbox"
            aria-label={"Enable " + p.name}
            checked={entry(p.id).enabled}
            onchange={(e) => setEnabled(p.id, e)}
          />
          <span></span>
        </span>
      </div>
      {#if needsKey(p)}
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
          oninput={(e) => (e.currentTarget.dataset.edited = "1")}
          onblur={(e) => saveKey(p.id, e)}
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
    padding: 2px 10px 8px;
    margin-bottom: 6px;
  }

  .prov .row {
    min-height: 32px;
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

  .hint {
    margin: 0 0 8px 2px;
    color: var(--faint);
  }
</style>
