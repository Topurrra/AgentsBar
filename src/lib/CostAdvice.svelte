<script>
  // The dollar advice card: spend prepaid balance before paying elsewhere, per
  // `costAdvice` in advisor.js. Healthy recedes — when there is nothing to say the
  // advice is [] and this renders nothing at all, no empty card.
  import { costAdvice } from "./advisor.js";
  import { usd } from "./format.js";

  let { snapshots } = $props();

  const advice = $derived(costAdvice(snapshots));
</script>

{#if advice.length}
  <div class="cost-advice">
    {#each advice as a (a.kind)}
      <!-- The amount leads and carries the colour: it is the number the sentence is
           about. The sentence itself stays plain text from advisor.js; `usd()` is the
           one place the figure is locale-formatted. -->
      <div class="advice {a.kind}">
        <span class="amount">{usd(a.amount)}</span>
        <span class="msg">{a.text}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  /* One quiet card holding every item, so the parent can drop it anywhere and the
     spacing between items stays this component's business. */
  .cost-advice {
    background: var(--surface-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: var(--sp-3) var(--sp-4);
  }

  .advice {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
  }

  .advice + .advice {
    margin-top: var(--sp-2);
  }

  .amount {
    flex: none;
    font-size: var(--type-body);
    font-weight: var(--weight-medium);
    font-variant-numeric: tabular-nums;
  }

  .msg {
    font-size: var(--type-body);
    line-height: var(--leading-body);
    color: var(--text-secondary);
  }

  /* Money already paid for is the good news and the thing to reach for, so it takes the
     calm colour — the same hairline used to mark the recommended provider. */
  .advice.balance {
    box-shadow: inset 2px 0 0 var(--state-calm-bar);
    padding-left: var(--sp-3);
  }

  .advice.balance .amount {
    color: var(--state-calm-text);
  }

  /* Money on its way out is a caution, not an alarm: amber at the number, muted in the
     sentence. */
  .advice.spend .amount {
    color: var(--state-watch-text);
  }
</style>
