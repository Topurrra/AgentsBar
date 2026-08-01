// Per-provider brand marks and accent colors, one source of truth for tiles and settings.
//
// The marks are the REAL vendor logos. Nine ship in full brand color, taken from the
// coloured originals in CodexBar's docs/logos/; the rest come from CodexBar's in-app
// icon set (Sources/CodexBar/Resources/ProviderIcon-<id>.svg). See Docs/icon-sources.md
// for per-file provenance and treatment. Each file keeps its own viewBox and its
// original path data. Trademarks belong to their respective owners.
//
// A mark either carries its own brand fills, or paints with currentColor, which
// ProviderIcon.svelte sets to the accent below. That is the fallback for brands whose
// own color is near-black (cursor, copilot, amp, factory, qwen) or that publish no
// coloured mark at all (windsurf, manus, augment). No mark is left invisible on #0f0f11.

const ACCENTS = {
  // Wave 1
  codex: "#7a9dff", // mid stop of the mark's own blue/violet gradient
  claude: "#d97757",
  gemini: "#4285f4",
  copilot: "#58a6ff",
  openai: "#74aa9c",
  zai: "#126ef6", // z.ai's own blue; the old #4f46e5 only just cleared 3:1
  minimax: "#e73562",
  kimi: "#fe603c",
  openrouter: "#94a3b8",
  deepseek: "#5786fe",
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
  qwen: "#cfcfd6", // stays monochrome: the shipped mark is #111111 and no coloured Qwen
  // Cloud variant exists in CodexBar's docs/logos. Docs/notes-logo-colour-decision.md.
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
