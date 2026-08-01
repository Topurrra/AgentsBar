<script>
  import { providerAccent, providerGlyph, providerLetter } from "./icons.js";

  let { id, size = 16 } = $props();

  const accent = $derived(providerAccent(id));
  const glyph = $derived(providerGlyph(id));
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  aria-hidden="true"
  focusable="false"
  style="color: {accent}"
>
  {#if glyph}
    <g
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- static local markup -->
      {@html glyph}
    </g>
  {:else}
    <rect
      x="1.5"
      y="1.5"
      width="21"
      height="21"
      rx="6"
      fill="currentColor"
      opacity="0.18"
    />
    <text
      x="12"
      y="12.6"
      fill="currentColor"
      font-size="13"
      font-weight="600"
      text-anchor="middle"
      dominant-baseline="central"
      font-family="inherit">{providerLetter(id)}</text
    >
  {/if}
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
