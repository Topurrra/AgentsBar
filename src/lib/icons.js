// Per-provider glyphs and accent colors, one source of truth for tiles and settings.
//
// The glyphs are ORIGINAL abstract marks drawn from generic geometry (chevrons, bars,
// circles, waves). They are not company logos and do not trace trademarked artwork.
// Providers with no obvious generic shape fall back to a lettermark, rendered by
// ProviderIcon.svelte. Every glyph is stroked with currentColor in a 24x24 viewBox.

const ICONS = {
  // Wave 1
  codex: {
    c: "#10a37f",
    g: '<path d="M6 8.5 9.5 12 6 15.5"/><path d="M12.5 16H18"/>',
  },
  claude: {
    c: "#d97757",
    g: '<path d="M12 4.5v15"/><path d="M5.5 8.25 18.5 15.75"/><path d="M18.5 8.25 5.5 15.75"/>',
  },
  gemini: {
    c: "#4285f4",
    g: '<path d="M12 4 20 12 12 20 4 12Z"/><circle cx="12" cy="12" r="2.2"/>',
  },
  copilot: {
    c: "#58a6ff",
    g: '<path d="M5 7.5 9.5 12 5 16.5"/><path d="M12.5 7.5 17 12l-4.5 4.5"/>',
  },
  openai: {
    c: "#74aa9c",
    g: '<path d="M12 3.5 19.5 7.8v8.4L12 20.5 4.5 16.2V7.8Z"/><circle cx="12" cy="12" r="2"/>',
  },
  zai: { c: "#4f46e5" },
  minimax: {
    c: "#f5462d",
    g: '<path d="M6 17v-3.5"/><path d="M12 17V7"/><path d="M18 17v-6.5"/>',
  },
  kimi: { c: "#6b8cff" },
  openrouter: {
    c: "#6467f2",
    g: '<circle cx="5.5" cy="12" r="1.8"/><circle cx="18.5" cy="6.5" r="1.8"/><circle cx="18.5" cy="17.5" r="1.8"/><path d="M7.3 11.2 16.8 7"/><path d="M7.3 12.8 16.8 17"/>',
  },
  deepseek: {
    c: "#4d6bfe",
    g: '<circle cx="12" cy="12" r="7.5"/><path d="M8.5 10.5 12 14l3.5-3.5"/>',
  },
  elevenlabs: {
    c: "#d0d0d8",
    g: '<path d="M8 8v8"/><path d="M12 5.5v13"/><path d="M16 8v8"/>',
  },
  deepgram: {
    c: "#13ef95",
    g: '<path d="M3.5 12c2-5.5 4.5-5.5 6.5 0s4.5 5.5 6.5 0"/><path d="M20 9.5v5"/>',
  },
  xai: { c: "#d8d8e0" },

  // Wave 2, cookie authenticated
  cursor: {
    c: "#cfcfd6",
    g: '<path d="M7 4.5 18.5 12.6l-5.2.8-2.6 4.6Z"/>',
  },
  factory: {
    c: "#f5713d",
    g: '<rect x="4" y="13" width="7" height="7" rx="1.4"/><rect x="13" y="13" width="7" height="7" rx="1.4"/><rect x="8.5" y="4" width="7" height="7" rx="1.4"/>',
  },
  devin: { c: "#2fb0c4" },
  t3chat: {
    c: "#e879a6",
    g: '<rect x="3.5" y="5" width="17" height="12" rx="3"/><path d="M9 17v3.2L13.2 17"/>',
  },
  opencode: {
    c: "#eab308",
    g: '<path d="M9 6.5 4 12l5 5.5"/><path d="M15 6.5 20 12l-5 5.5"/>',
  },
  manus: { c: "#c2a878" },
  warp: {
    c: "#00c8ff",
    g: '<circle cx="12" cy="12" r="8"/><path d="M9.5 8.5 13.5 12l-4 3.5"/>',
  },
  windsurf: {
    c: "#3fb984",
    g: '<path d="M12 3.5V14"/><path d="M12 4.5 18.5 10 12 12.5"/><path d="M4 17.5c2.5-2 5-2 7.5 0s5 2 7.5 0"/>',
  },
  augment: {
    c: "#8b5cf6",
    g: '<circle cx="12" cy="12" r="8"/><path d="M12 8v8"/><path d="M8 12h8"/>',
  },
  amp: {
    c: "#ff6b35",
    g: '<path d="M13.5 3.5 6.5 13h5l-1 7.5L18 11h-5.2Z"/>',
  },
};

export const NEUTRAL_ACCENT = "#8b8b95";

/// Brand accent color for a provider id, neutral gray for unknown ids.
export function providerAccent(id) {
  return ICONS[id]?.c ?? NEUTRAL_ACCENT;
}

/// Inline SVG body for a provider id, or null when it uses a lettermark.
export function providerGlyph(id) {
  return ICONS[id]?.g ?? null;
}

/// Lettermark character for a provider id.
export function providerLetter(id) {
  const c = String(id ?? "").trim()[0];
  return c ? c.toUpperCase() : "?";
}

// Registry order, mirrors providers/mod.rs all_providers(). Used by the icon test.
export const PROVIDER_IDS = Object.keys(ICONS);
