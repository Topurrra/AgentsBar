// The Advisor's trend readings over the usage history: is this provider burning more than
// last week, and is the current burn on track to exhaust the quota. Pure functions over
// the sample series so `node src/lib/trends.test.js` can prove them without a DOM.
//
// Sample shape is the history buffer's: `{ t: unix seconds, u: used percent 0..100,
// l?: lane }`, appended in time order. Every `now` argument here is unix seconds too, so
// the maths stays in the samples' domain; the UI divides its millisecond clock by 1000
// before calling.
//
// The suppression rules ARE the feature, same as `pace` in tiles.js: a trend line that
// appears on two data points or a flat series teaches the user that the line means
// nothing. Every function returns null rather than a hedge.

const DAY = 86400;
const WEEK = 7 * DAY;

// --- week over week -----------------------------------------------------------------

// Fewer points than this in a half and the "average" is a single sample wearing a costume.
const MIN_WINDOW_SAMPLES = 2;
// A delta inside ±1 point is noise, not a direction. Same idea as pace's band.
const WOW_FLAT_BAND = 1;

// Only finite points count; one NaN must not poison an average. u is clamped to [0,100]
// so a bad reading cannot pull a mean outside what the quantity even means.
function points(samples) {
  if (!Array.isArray(samples)) return [];
  return samples.filter((s) => Number.isFinite(s?.t) && Number.isFinite(s?.u));
}

const clampU = (u) => Math.max(0, Math.min(100, u));

// Average used percent across one time window, or null when it holds too few samples to
// be honest. `endInclusive` exists so the two week halves can share a boundary without
// ever double-counting the sample that sits exactly on it.
function avgIn(pts, from, to, endInclusive) {
  let sum = 0;
  let n = 0;
  for (const s of pts) {
    const inside = endInclusive ? s.t >= from && s.t <= to : s.t >= from && s.t < to;
    if (!inside) continue;
    sum += clampU(s.u);
    n += 1;
  }
  return n >= MIN_WINDOW_SAMPLES ? sum / n : null;
}

// This week's average usage against last week's, in percentage points. "Up" means more
// of the quota is being consumed, which is the worse direction: the sign carries the
// meaning, `direction` only names it. The current half is `[now-7d, now]` and the
// previous half `[now-14d, now-7d)`; a boundary sample belongs to THIS week, so the two
// halves never share a point.
//
// Short histories degrade honestly: whatever falls inside the fixed halves is compared,
// and a half with fewer than two samples refuses the comparison (null) instead of
// pretending one point is a week. Samples newer than `now` (clock skew) count for
// nothing, the same as future data everywhere else.
export function weekOverWeek(samples, now) {
  if (!Number.isFinite(now)) return null;
  const pts = points(samples);
  if (!pts.length) return null;
  const current = avgIn(pts, now - WEEK, now, true);
  const previous = avgIn(pts, now - 2 * WEEK, now - WEEK, false);
  if (current === null || previous === null) return null;
  const deltaPct = current - previous;
  const direction =
    deltaPct > WOW_FLAT_BAND ? "up" : deltaPct < -WOW_FLAT_BAND ? "down" : "flat";
  return { deltaPct, direction };
}

// --- forecast -----------------------------------------------------------------------

// The fit only looks at the recent burn; a week-old plateau is not today's story.
const FIT_WINDOW = 2 * DAY;
// Two points are always a perfect line, which is exactly the problem: one jittery
// reading becomes a slope. Three is the first honest fit.
const MIN_FIT_SAMPLES = 3;
// Below this much time between the first and last fitted point, least squares amplifies
// a single blip into a direction.
const MIN_FIT_SPAN = 3600;
// Further out than this and the projection is weather forecasting, not advice.
const HORIZON = 14 * DAY;

// When the recent trend reaches 100% used, as epoch milliseconds (so the caller can hand
// it straight to `Date` or `countdown`), or null when there is nothing worth saying:
// the trend is flat or falling, the fit is dishonest (too few points, too short a span),
// the quota is already exhausted, or the crossing lies beyond the horizon.
//
// The math is plain least squares over `(t, u)` in the last 48h, evaluated at `now`:
// the line that summarises the recent burn, extended forward. Honest, and deliberately
// nothing fancier.
export function forecast(samples, now) {
  if (!Number.isFinite(now)) return null;
  const recent = points(samples).filter((s) => s.t <= now && s.t >= now - FIT_WINDOW);
  if (recent.length < MIN_FIT_SAMPLES) return null;

  const us = recent.map((s) => clampU(s.u));
  let t0 = Infinity;
  let t1 = -Infinity;
  let tSum = 0;
  let uSum = 0;
  recent.forEach((s, i) => {
    if (s.t < t0) t0 = s.t;
    if (s.t > t1) t1 = s.t;
    tSum += s.t;
    uSum += us[i];
  });
  if (t1 - t0 < MIN_FIT_SPAN) return null;

  const tBar = tSum / recent.length;
  const uBar = uSum / recent.length;
  let num = 0;
  let den = 0;
  recent.forEach((s, i) => {
    num += (s.t - tBar) * (us[i] - uBar);
    den += (s.t - tBar) * (s.t - tBar);
  });
  if (!(den > 0)) return null;
  const slope = num / den; // percentage points per second

  // Flat or falling: nothing runs out on this trajectory.
  if (!(slope > 0)) return null;

  const uNow = uBar + slope * (now - tBar);
  // Already there: the tile says so, and a forecast would only repeat it.
  if (uNow >= 100) return null;

  const hitsAt = now + (100 - uNow) / slope;
  if (hitsAt > now + HORIZON) return null;
  return Math.round(hitsAt * 1000);
}

// --- history lookup -----------------------------------------------------------------

// One provider's samples, whole. History is keyed by SERIES (`provider:account` when the
// provider has several accounts, bare provider id otherwise — see multiAccountProviders
// in tiles.js), so a provider's points may live under several keys. Concatenate them all
// and order by time: the trend maths does not care which account a point came from, only
// that it is this provider's usage. Prefix match is on `id + ":"` exactly, so "claude"
// never swallows a hypothetical "claude-extra" provider.
export function samplesFor(history, providerId) {
  if (!history || typeof history !== "object") return [];
  if (typeof providerId !== "string" || !providerId) return [];
  const out = [];
  for (const [key, series] of Object.entries(history)) {
    if (key !== providerId && !key.startsWith(providerId + ":")) continue;
    if (Array.isArray(series)) out.push(...series);
  }
  return out.sort((a, b) => (a?.t ?? 0) - (b?.t ?? 0));
}
