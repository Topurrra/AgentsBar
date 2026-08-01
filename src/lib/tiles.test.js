// Run: node src/lib/tiles.test.js
import assert from "node:assert/strict";
import { urgency, sortTiles, oldestFetch, ago } from "./tiles.js";

const t0 = Date.parse("2026-01-01T00:00:00Z");
const iso = (min) => new Date(t0 + min * 60000).toISOString();

const win = (label, used, minutes, resets = null) => ({
  label,
  used_percent: used,
  window_minutes: minutes,
  resets_at: resets,
});

// --- row 15: urgency order -------------------------------------------------------

const prov = (id, configured = true) => ({ id, configured });
// The backend sends `windows` already clamped by `state::display_windows`; the raw
// lane fields are deliberately not what the tile reads.
const snap = (windows, error = null) => ({ windows, error });

assert.equal(urgency(prov("a"), snap([win("5h", 100, 300)])), 0, "exhausted");
assert.equal(urgency(prov("a"), snap([win("5h", 30, 300)])), 70);
assert.equal(urgency(prov("a"), snap([])), 200, "no data ranks below any number");
assert.equal(
  urgency(prov("a"), snap([win("5h", null, 300)])),
  200,
  "unknown is not 0% used",
);
assert.equal(urgency(prov("a", false), snap([])), 300, "unconfigured");
assert.equal(urgency(prov("a"), snap([], "boom")), 400, "errored is last");
assert.equal(urgency(prov("a", false), snap([], "boom")), 300, "unconfigured beats errored");
assert.equal(urgency(prov("a"), snap([win("5h", 0, 300)]), "a"), -1, "pinned wins");

// A fresh 5h above an exhausted weekly must rank as exhausted, not as healthy. The
// backend clamps the 5h to 100 before it gets here, which is what makes this 0.
assert.equal(
  urgency(prov("a"), snap([win("5h", 100, 300), win("Weekly", 100, 10080)])),
  0,
);

{
  const providers = [
    prov("healthy"),
    prov("errored"),
    prov("nodata"),
    prov("unconfigured", false),
    prov("low"),
    prov("dead"),
    prov("pinme"),
  ];
  const byId = new Map([
    ["healthy", snap([win("5h", 10, 300)])],
    ["errored", snap([win("5h", 5, 300)], "boom")],
    ["nodata", snap([])],
    ["unconfigured", undefined],
    ["low", snap([win("5h", 85, 300)])],
    ["dead", snap([win("5h", 100, 300)])],
    ["pinme", snap([win("5h", 2, 300)])],
  ]);
  assert.deepEqual(
    sortTiles(providers, byId, "pinme").map((p) => p.id),
    ["pinme", "dead", "low", "healthy", "nodata", "unconfigured", "errored"],
  );
  // Stability: same urgency keeps registry order.
  const tied = [prov("x"), prov("y")];
  const tiedIds = new Map([
    ["x", snap([win("5h", 50, 300)])],
    ["y", snap([win("5h", 50, 300)])],
  ]);
  assert.deepEqual(sortTiles(tied, tiedIds, null).map((p) => p.id), ["x", "y"]);
}

// --- row 20: freshness -----------------------------------------------------------

assert.equal(
  oldestFetch([{ fetched_at: iso(0) }, { fetched_at: iso(10) }]),
  iso(0),
  "oldest, not newest",
);
assert.equal(oldestFetch([{ fetched_at: iso(5) }, {}]), iso(5), "a missing stamp is skipped");
assert.equal(oldestFetch([]), null);

assert.equal(ago(iso(0), t0 + 30 * 1000), "just now");
assert.equal(ago(iso(0), t0 + 4 * 60000), "4m ago");
assert.equal(ago(iso(0), t0 + 130 * 60000), "2h ago");
assert.equal(ago(iso(0), t0 + 3 * 1440 * 60000), "3d ago");
assert.equal(ago(null, t0), "");

console.log("tiles.js ok");
