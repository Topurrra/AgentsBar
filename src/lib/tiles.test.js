// Run: node src/lib/tiles.test.js
import assert from "node:assert/strict";
import { urgency, sortTiles, oldestFetch, ago, pace, errorCopy } from "./tiles.js";

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

// --- row 26: pace ------------------------------------------------------------------

// A window that resets `mins` from now, `minutes` long, `used` percent consumed.
const w = (used, minutes, mins) =>
  win("W", used, minutes, new Date(t0 + mins * 60000).toISOString());

const HOUR = 60;
const WEEK = 7 * 24 * HOUR; // 10080 minutes

// Suppression: nothing to say, so nothing is said.
assert.equal(pace(w(null, WEEK, 3.5 * 24 * HOUR), t0), null, "unknown percent");
assert.equal(pace(w(undefined, WEEK, 3.5 * 24 * HOUR), t0), null, "missing percent");
assert.equal(pace(win("W", 50, WEEK, null), t0), null, "no reset time");
assert.equal(pace(win("W", 50, WEEK, "not a date"), t0), null, "unparseable reset time");
assert.equal(pace(win("W", 50, null, new Date(t0 + 3600000).toISOString()), t0), null, "no window length");
assert.equal(pace(w(50, 0, 60), t0), null, "zero length window");
assert.equal(pace(w(50, WEEK, 0), t0), null, "reset due now");
assert.equal(pace(w(50, WEEK, -60), t0), null, "reset in the past");
assert.equal(
  pace(w(50, 300, 6 * HOUR), t0),
  null,
  "reset further out than the window is long: the fields disagree",
);
assert.equal(pace(w(100, WEEK, 3.5 * 24 * HOUR), t0), null, "exhausted");
assert.equal(pace(w(140, WEEK, 3.5 * 24 * HOUR), t0), null, "over 100 is still exhausted");

// The 3 percent elapsed floor, on a 5h window (300 min).
assert.equal(pace(w(10, 300, 294), t0), null, "2% elapsed is too early to mean anything");
assert.notEqual(pace(w(10, 300, 291), t0), null, "3% elapsed is the first that counts");

// Ahead, behind, on pace. Half a week gone in every case below.
const half = (used) => pace(w(used, WEEK, 3.5 * 24 * HOUR), t0);
assert.equal(half(50).label, "On pace");
assert.equal(half(50).state, "even");
assert.equal(half(52).label, "On pace", "within the 2 point band");
assert.equal(half(48).label, "On pace", "band is symmetric");
assert.equal(half(53).label, "3% in deficit");
assert.equal(half(53).state, "deficit");
assert.equal(half(70).label, "20% in deficit");
assert.equal(half(30).label, "20% in reserve");
assert.equal(half(30).state, "reserve");
assert.equal(half(0).label, "50% in reserve", "untouched window is the whole reserve");

// The eta half.
assert.equal(half(50).eta, "Lasts until reset", "exactly on pace lands on the reset");
// The two halves must never argue. Inside the band the label says "On pace" while the
// projection undershoots the reset by those same couple of points, so that half is not
// said at all rather than rendered as "On pace · Runs out in 3d 8h".
for (const used of [50.5, 51, 52]) {
  assert.equal(half(used).label, "On pace", used);
  assert.equal(half(used).eta, null, used);
}
// Ahead inside the band still says so: true and worth saying.
assert.equal(half(49).eta, "Lasts until reset");
// One point past the band the eta speaks again, because the label now agrees with it.
assert.equal(half(53).eta, "Runs out in 3d 2h");
// Nearly exhausted with minutes to go is on pace by the band, and the same rule applies:
// the tile must not claim the window lasts when the projection says it does not.
assert.equal(pace(w(99.9, 300, 3), t0).state, "even");
assert.equal(pace(w(99.9, 300, 3), t0).eta, null);
assert.equal(half(30).eta, "Lasts until reset");
assert.equal(half(0).eta, "Lasts until reset", "no burn, no run out");
// 84h left, 75 used in 84h: 25 left at 75 per 84h runs 28h.
assert.equal(half(75).eta, "Runs out in 1d 4h");
// 1h left of a 5h window, 99.9 used: 20 points of deficit, and gone in seconds.
assert.equal(pace(w(99.9, 300, 60), t0).eta, "Runs out now");
// 5h window, 1h left, 95 used: 5 left at that rate is 12 minutes.
assert.equal(pace(w(95, 300, 60), t0).label, "15% in deficit");
assert.equal(pace(w(95, 300, 60), t0).eta, "Runs out in 12m");

// --- rows 21 and 22: error copy -------------------------------------------------------

const cookieProv = { id: "cursor", name: "Cursor", auth: "cookie", doc_url: "https://cursor.com/dashboard?tab=usage" };
const keyProv = { id: "openrouter", name: "OpenRouter", auth: "api_key", doc_url: "https://openrouter.ai/settings/keys" };
const oauthProv = { id: "codex", name: "Codex", auth: "oauth_file", doc_url: "https://developers.openai.com/codex" };
const err = (kind, message = "boom") => ({ error: message, error_kind: kind });

assert.equal(errorCopy(cookieProv, { error: null }), null, "no error, no copy");
assert.equal(errorCopy(cookieProv, undefined), null, "no snapshot, no copy");

// The self-caused case beats every kind: Retry cannot fix a switch the user turned off.
{
  const c = errorCopy(cookieProv, err("auth"), "off");
  assert.equal(c.text, "Cookie import is off for this provider. Turn it on in Settings.");
  assert.equal(c.retry, false);
  assert.equal(c.tone, "warn");
}
assert.notEqual(errorCopy(keyProv, err("auth"), "off").tone, "warn", "not a cookie provider");

// Row 21 UI half: auth is red and asks for something, transient is muted and silent.
assert.equal(errorCopy(cookieProv, err("auth")).tone, "bad");
assert.equal(errorCopy(cookieProv, err("http")).tone, "muted");
assert.equal(errorCopy(cookieProv, err("parse")).tone, "muted");
assert.equal(errorCopy(cookieProv, err("rate_limited")).tone, "muted");
assert.equal(errorCopy(cookieProv, err("not_configured")).tone, "warn");

// What failed, what to do, where.
assert.equal(
  errorCopy(cookieProv, err("auth")).text,
  "Your Cursor session expired. Sign in at cursor.com in your browser, then Retry.",
);
assert.equal(
  errorCopy(keyProv, err("auth")).text,
  "OpenRouter rejected the saved key. Get a new one at openrouter.ai and paste it in Settings.",
);
assert.equal(
  errorCopy(oauthProv, err("auth")).text,
  "Codex sign-in expired. Sign in again with the Codex CLI, then Retry.",
);
// Row 38. A key that came from the environment is not visible in Settings, so the copy
// names the variable rather than sending the user to a field that will not change it.
assert.equal(
  errorCopy({ ...keyProv, env_key: "OPENROUTER_API_KEY" }, err("auth")).text,
  "OpenRouter rejected the saved key. Get a new one at openrouter.ai and paste it in Settings, or in OPENROUTER_API_KEY if you set it there.",
);
assert.match(errorCopy(cookieProv, err("not_configured")).text, /No Cursor session found/);
assert.match(errorCopy(keyProv, err("not_configured")).text, /No API key for OpenRouter/);
assert.match(errorCopy(cookieProv, err("rate_limited")).text, /Waiting before the next check/);
assert.match(errorCopy(keyProv, err("parse")).text, /could not read/);

// The raw status never reaches the tile as the message.
for (const kind of ["auth", "http", "parse", "rate_limited", "not_configured"]) {
  assert.doesNotMatch(errorCopy(keyProv, err(kind, "HTTP 429")).text, /HTTP/, kind);
}

// A wave 3 payload has no kind. Transient, so a guess never paints a tile red.
assert.equal(errorCopy(keyProv, { error: "HTTP 502" }).tone, "muted");
assert.equal(errorCopy(keyProv, { error: "HTTP 502" }).text, "Could not reach OpenRouter. Retrying on the next refresh.");
// A provider with no doc_url still produces a sentence, just without the "where".
assert.equal(
  errorCopy({ name: "Nowhere", auth: "api_key" }, err("auth")).text,
  "Nowhere rejected the saved key. Get a new one and paste it in Settings.",
);

console.log("tiles.js ok");
