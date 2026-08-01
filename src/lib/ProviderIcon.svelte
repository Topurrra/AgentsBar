<script>
  import { providerAccent, providerLogoName, providerLetter } from "./icons.js";

  // Bundled at build time, no runtime fetch. Each file keeps its own viewBox; the
  // rendered size comes from CSS below, so no path data is ever rescaled.
  const LOGOS = import.meta.glob("./logos/*.svg", {
    query: "?raw",
    import: "default",
    eager: true,
  });

  let { id, size = 16 } = $props();

  const accent = $derived(providerAccent(id));
  const logo = $derived(LOGOS[`./logos/${providerLogoName(id)}.svg`] ?? null);
</script>

{#if logo}
  <span class="logo" style="--s: {size}px; color: {accent}" aria-hidden="true"
    ><!-- eslint-disable-next-line svelte/no-at-html-tags -- bundled local asset -->
    {@html logo}</span
  >
{:else}
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    aria-hidden="true"
    focusable="false"
    style="color: {accent}"
  >
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
  </svg>
{/if}

<style>
  svg {
    flex: none;
    display: block;
  }

  .logo {
    flex: none;
    display: block;
    width: var(--s);
    height: var(--s);
  }

  /* Overrides each file's own width/height attributes without touching its viewBox. */
  .logo :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }
</style>
