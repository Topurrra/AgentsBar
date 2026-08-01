// Run: node src/lib/icons.test.js
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  PROVIDER_IDS,
  NEUTRAL_ACCENT,
  providerAccent,
  providerGlyph,
  providerLetter,
} from "./icons.js";

// Every provider in the Rust registry must have an entry here, and nothing extra.
const registry = readFileSync(
  new URL("../../src-tauri/src/providers/mod.rs", import.meta.url),
  "utf8",
);
const block = registry.slice(
  registry.indexOf("pub fn all_providers()"),
  registry.indexOf("pub fn provider_by_id"),
);
const rustIds = [...block.matchAll(/Box::new\((\w+)::/g)].map((m) => m[1]);
assert.equal(rustIds.length, 23, "expected 23 registered providers");
assert.deepEqual([...PROVIDER_IDS].sort(), [...rustIds].sort());

for (const id of PROVIDER_IDS) {
  const accent = providerAccent(id);
  assert.match(accent, /^#[0-9a-f]{6}$/, `${id} needs a hex accent`);
  assert.notEqual(accent, NEUTRAL_ACCENT, `${id} needs a brand accent`);

  const g = providerGlyph(id);
  if (g === null) {
    // Lettermark fallback.
    assert.match(providerLetter(id), /^[A-Z]$/);
  } else {
    assert.match(g, /^<(path|circle|rect)/, `${id} glyph must be svg markup`);
    // Path data must only use commands we intend, so a typo cannot render blank.
    for (const [, d] of g.matchAll(/ d="([^"]*)"/g)) {
      assert.match(d, /^[MmLlHhVvCcSsZ0-9.\-\s]+$/, `${id} bad path data`);
      assert.match(d, /^M/, `${id} path must start with a moveto`);
    }
    assert.ok(!/</.test(g.replace(/<\/?(path|circle|rect)[^>]*>/g, "")), `${id} unexpected tag`);
    // Stay inside the 24x24 viewBox. Relative path commands can be negative deltas,
    // so the bound is the box size in either direction.
    for (const n of g.match(/-?\d+(\.\d+)?/g) ?? []) {
      assert.ok(Math.abs(Number(n)) <= 24, `${id} coord ${n} out of viewBox`);
    }
  }
}

assert.equal(providerAccent("nope"), NEUTRAL_ACCENT);
assert.equal(providerGlyph("nope"), null);
assert.equal(providerLetter("nope"), "N");
assert.equal(providerLetter(""), "?");
assert.equal(providerLetter(undefined), "?");

console.log(`icons.js ok (${PROVIDER_IDS.length} providers)`);
