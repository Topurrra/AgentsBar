// Per-provider brand marks and accent colors, one source of truth for tiles and settings.
//
// The marks are the REAL vendor logos, copied from CodexBar's in-app icon set
// (Sources/CodexBar/Resources/ProviderIcon-<id>.svg) into src/lib/logos/. See
// Docs/icon-sources.md for per-file provenance. Each file keeps its own viewBox and
// its original path data; only two near-black fills were swapped to currentColor so
// they are visible on our dark popover. Trademarks belong to their respective owners.
//
// About half the marks are solid white and ignore the accent; the rest paint with
// currentColor, which ProviderIcon.svelte sets to the accent below.

const ACCENTS = {
  // Wave 1
  codex: "#10a37f",
  claude: "#d97757",
  gemini: "#4285f4",
  copilot: "#58a6ff",
  openai: "#74aa9c",
  zai: "#4f46e5",
  minimax: "#f5462d",
  kimi: "#6b8cff",
  openrouter: "#6467f2",
  deepseek: "#4d6bfe",
  elevenlabs: "#d0d0d8",
  deepgram: "#13ef95",
  xai: "#d8d8e0",

  // Wave 2, cookie authenticated
  cursor: "#cfcfd6",
  factory: "#f5713d",
  devin: "#2fb0c4",
  t3chat: "#e879a6",
  opencode: "#eab308",
  manus: "#c2a878",
  warp: "#00c8ff",
  windsurf: "#3fb984",
  augment: "#8b5cf6",
  amp: "#ff6b35",
};

// openai ships no mark of its own. CodexBar's OpenAI API and Azure OpenAI descriptors
// both point at ProviderIcon-codex, so one OpenAI mark serves both products.
const LOGO_ALIAS = { openai: "codex" };

export const NEUTRAL_ACCENT = "#8b8b95";

/// Brand accent color for a provider id, neutral gray for unknown ids.
export function providerAccent(id) {
  return ACCENTS[id] ?? NEUTRAL_ACCENT;
}

/// Basename of the logo file in src/lib/logos/ for a provider id.
export function providerLogoName(id) {
  return LOGO_ALIAS[id] ?? id;
}

/// Lettermark character, used only when a provider has no logo file.
export function providerLetter(id) {
  const c = String(id ?? "").trim()[0];
  return c ? c.toUpperCase() : "?";
}

// Registry order, mirrors providers/mod.rs all_providers(). Used by the icon test.
export const PROVIDER_IDS = Object.keys(ACCENTS);
