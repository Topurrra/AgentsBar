<script>
  import { percentLeft, tone, countdown, credits } from "./format.js";

  let { provider, snapshot, now, onRetry } = $props();

  const windows = $derived(
    [snapshot?.primary, snapshot?.secondary, snapshot?.tertiary].filter(Boolean),
  );
  const balance = $derived(credits(snapshot?.credits));
</script>

<div class="tile" class:failed={!!snapshot?.error}>
  <div class="head">
    <span class="name">{provider.name}</span>
    {#if snapshot?.plan}<span class="badge">{snapshot.plan}</span>{/if}
    <span class="gap"></span>
    {#if snapshot?.account}
      <span class="account" title={snapshot.account}>{snapshot.account}</span>
    {/if}
  </div>

  {#if snapshot?.error}
    <div class="error">
      <span class="msg" title={snapshot.error}>{snapshot.error}</span>
      <button class="retry" onclick={() => onRetry(provider.id)}>Retry</button>
    </div>
  {/if}

  {#each windows as w}
    {@const left = percentLeft(w)}
    <div class="win">
      <div class="winhead">
        <span class="label">{w.label}</span>
        <span class="gap"></span>
        <span class="pct {tone(left)}">{left === null ? "--" : left + "% left"}</span>
        {#if w.resets_at}
          <span class="reset">{countdown(w.resets_at, now)}</span>
        {/if}
      </div>
      <div class="track">
        <div class="fill {tone(left)}" style="width: {left ?? 0}%"></div>
      </div>
    </div>
  {/each}

  {#if balance !== null}
    <div class="credits">Credits <b>{balance}</b></div>
  {/if}

  {#if !snapshot && !windows.length}
    <div class="idle">
      {provider.configured ? "Waiting for first refresh" : "Not configured"}
    </div>
  {:else if !windows.length && !snapshot?.error && balance === null}
    <div class="idle">No usage reported</div>
  {/if}
</div>

<style>
  .tile {
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--panel);
    padding: 9px 10px 10px;
  }

  .tile.failed {
    border-color: rgba(229, 83, 75, 0.35);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .gap {
    flex: 1;
  }

  .name {
    font-weight: 600;
    letter-spacing: 0.1px;
  }

  .badge {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.4px;
    color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 1px 5px;
  }

  .account {
    font-size: 11px;
    color: var(--faint);
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .error {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 7px;
    font-size: 11.5px;
    color: var(--bad);
  }

  .msg {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .retry {
    flex: none;
    color: var(--bad);
    border: 1px solid rgba(229, 83, 75, 0.4);
    border-radius: 5px;
    padding: 1px 7px;
    font-size: 11px;
    transition: background 0.12s ease;
  }

  .retry:hover {
    background: rgba(229, 83, 75, 0.14);
  }

  .win {
    margin-top: 8px;
  }

  .winhead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 11.5px;
    margin-bottom: 4px;
  }

  .label {
    color: var(--dim);
  }

  .pct {
    font-variant-numeric: tabular-nums;
  }

  .reset {
    color: var(--faint);
    font-variant-numeric: tabular-nums;
    min-width: 42px;
    text-align: right;
  }

  .track {
    height: 5px;
    border-radius: 3px;
    background: #26262c;
    overflow: hidden;
  }

  .fill {
    height: 100%;
    border-radius: 3px;
    transition: width 0.25s ease;
  }

  .ok {
    color: var(--ok);
  }
  .warn {
    color: var(--warn);
  }
  .bad {
    color: var(--bad);
  }
  .unknown {
    color: var(--faint);
  }

  .fill.ok {
    background: var(--ok);
  }
  .fill.warn {
    background: var(--warn);
  }
  .fill.bad {
    background: var(--bad);
  }
  .fill.unknown {
    background: #3a3a42;
  }

  .credits {
    margin-top: 8px;
    font-size: 11.5px;
    color: var(--dim);
  }

  .credits b {
    color: var(--text);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .idle {
    margin-top: 7px;
    font-size: 11.5px;
    color: var(--faint);
  }
</style>
