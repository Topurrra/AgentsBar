// Run: node src/lib/format.test.js
import assert from "node:assert/strict";
import { percentLeft, tone, countdown, credits, usd } from "./format.js";

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

// usd is locale-formatted, so match the digits rather than a fixed "$" string.
assert.equal(usd(null), null);
assert.equal(usd(NaN), null);
assert.equal(usd(Infinity), null);
assert.match(usd(12.5), /12[.,]50/);
assert.match(usd(1000), /1[.,]000[.,]00/);

console.log("format.js ok");
