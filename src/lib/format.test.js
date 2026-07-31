// Run: node src/lib/format.test.js
import assert from "node:assert/strict";
import { percentLeft, tone, countdown, credits, lastRefresh } from "./format.js";

assert.equal(percentLeft({ used_percent: 43 }), 57);
assert.equal(percentLeft({ used_percent: 120 }), 0);
assert.equal(percentLeft({ used_percent: -5 }), 100);
assert.equal(percentLeft({}), null);

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

assert.equal(
  lastRefresh([{ fetched_at: iso(0) }, { fetched_at: iso(10) }]),
  iso(10),
);
assert.equal(lastRefresh([]), null);

console.log("format.js ok");
