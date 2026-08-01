// Run: node src/lib/format.test.js
import assert from "node:assert/strict";
import { percentLeft, tone, countdown, credits } from "./format.js";

assert.equal(percentLeft({ used_percent: 43 }), 57);
assert.equal(percentLeft({ used_percent: 120 }), 0);
assert.equal(percentLeft({ used_percent: -5 }), 100);
assert.equal(percentLeft({ used_percent: 0 }), 100);
assert.equal(percentLeft({}), null);
// An unknown window is JSON null and must not read as a confident "100% left".
assert.equal(percentLeft({ used_percent: null }), null);
assert.equal(percentLeft({ used_percent: undefined }), null);
assert.equal(percentLeft({ used_percent: NaN }), null);
assert.equal(percentLeft({ used_percent: Infinity }), null);
assert.equal(percentLeft({ used_percent: "43" }), null);
assert.equal(percentLeft(null), null);
// tone() turns that null into "unknown", never into a green bar.
assert.equal(tone(percentLeft({ used_percent: null })), "unknown");

assert.equal(tone(51), "ok");
assert.equal(tone(50), "warn");
assert.equal(tone(20), "warn");
assert.equal(tone(19), "bad");
assert.equal(tone(null), "unknown");

const t0 = Date.parse("2026-01-01T00:00:00Z");
const iso = (min) => new Date(t0 + min * 60000).toISOString();
assert.equal(countdown(iso(134), t0), "2h 14m");
assert.equal(countdown(iso(45), t0), "45m");
assert.equal(countdown(iso(1500), t0), "1d 1h");
assert.equal(countdown(iso(-1), t0), "now");
assert.equal(countdown(null, t0), "");

assert.equal(credits(12.345), "12.35");
assert.equal(credits(null), null);

console.log("format.js ok");
