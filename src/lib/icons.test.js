// Run: node src/lib/icons.test.js
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import {
  PROVIDER_IDS,
  NEUTRAL_ACCENT,
  providerAccent,
  providerLogoName,
  providerLetter,
} from "./icons.js";

// Every provider in the Rust registry must have an entry here, and nothing extra.
// No hardcoded count: the registry is the source of truth (wave 3, row 39).
const registry = readFileSync(
  new URL("../../src-tauri/src/providers/mod.rs", import.meta.url),
  "utf8",
);
const block = registry.slice(
  registry.indexOf("pub fn all_providers()"),
  registry.indexOf("pub fn provider_by_id"),
);
const rustIds = [...block.matchAll(/Box::new\((\w+)::/g)].map((m) => m[1]);
assert.ok(rustIds.length > 0, "could not parse all_providers()");
assert.deepEqual([...PROVIDER_IDS].sort(), [...rustIds].sort());

const logoDir = new URL("./logos/", import.meta.url);
const files = readdirSync(logoDir);

// WCAG relative luminance and contrast against the popover background, so a mark that
// would be invisible on #0f0f11 fails here instead of shipping.
const POPOVER_BG = "#0f0f11";

function luminance(hex) {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function tooDark(hex) {
  const [hi, lo] = [luminance(hex), luminance(POPOVER_BG)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05) < 3;
}

for (const id of PROVIDER_IDS) {
  const accent = providerAccent(id);
  assert.match(accent, /^#[0-9a-f]{6}$/, `${id} needs a hex accent`);
  assert.notEqual(accent, NEUTRAL_ACCENT, `${id} needs a brand accent`);
  assert.ok(!tooDark(accent), `${id} accent ${accent} is too dark for #0f0f11`);

  // Row 39: every registered provider resolves to a real logo file.
  const name = `${providerLogoName(id)}.svg`;
  assert.ok(files.includes(name), `${id} has no logo file (${name})`);

  const svg = readFileSync(new URL(name, logoDir), "utf8");
  assert.match(svg, /^<svg[\s>]/, `${name} must start with <svg>`);
  assert.match(svg, /viewBox="/, `${name} must keep its own viewBox`);
  assert.ok(!/<script/i.test(svg), `${name} must not contain script`);
  assert.ok(!/<!--/.test(svg), `${name} still has comments`);
  assert.ok(!/<title/i.test(svg), `${name} still has a title element`);
  assert.ok(!/<\?xml/.test(svg), `${name} still has an XML preamble`);

  // Something must actually be painted light, or the mark is a blank square.
  assert.match(
    svg,
    /(fill|stroke)="(white|currentColor)"/,
    `${name} paints nothing visible`,
  );

  // No painted geometry may be near-black: those marks are invisible on our surface.
  // clipPath fills are masks, not paint, so exclude that subtree.
  const painted = svg.replace(/<clipPath[\s\S]*?<\/clipPath>/g, "");
  for (const [, attr, hex] of painted.matchAll(
    /(fill|stroke)="(#[0-9a-fA-F]{6})"/g,
  )) {
    assert.ok(!tooDark(hex), `${name} ${attr}=${hex} is invisible on #0f0f11`);
  }
}

// No stray logo files: every file belongs to some provider.
const wanted = new Set(PROVIDER_IDS.map((id) => `${providerLogoName(id)}.svg`));
for (const f of files) assert.ok(wanted.has(f), `${f} belongs to no provider`);

assert.equal(providerAccent("nope"), NEUTRAL_ACCENT);
assert.equal(providerLogoName("nope"), "nope");
assert.equal(providerLogoName("openai"), "codex");
assert.equal(providerLetter("nope"), "N");
assert.equal(providerLetter(""), "?");
assert.equal(providerLetter(undefined), "?");

console.log(
  `icons.js ok (${PROVIDER_IDS.length} providers, ${files.length} logo files)`,
);
