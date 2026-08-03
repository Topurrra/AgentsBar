// Run: node src/lib/trends.test.js
import assert from "node:assert/strict";
import { weekOverWeek, forecast, samplesFor } from "./trends.js";

// Sample timestamps are unix seconds; pick a round clock and measure everything in days
// relative to it.
const NOW = 1_000_000;
const DAY = 86400;
const WEEK = 7 * DAY;

// A sample `daysAgo` days before NOW, `u` percent used.
const s = (daysAgo, u) => ({ t: NOW - daysAgo * DAY, u });

// --- week over week -----------------------------------------------------------------

{
  // A quiet previous week and a busy current one: usage went up.
  const samples = [
    s(13, 30), s(9, 30), // previous week averages 30
    s(5, 48), s(1, 48), // this week averages 48
  ];
  assert.deepEqual(weekOverWeek(samples, NOW), { deltaPct: 18, direction: "up" });
}

{
  // The mirror image: busy last week, quiet this week, usage went down.
  const samples = [
    s(13, 48), s(9, 48),
    s(5, 30), s(1, 30),
  ];
  assert.deepEqual(weekOverWeek(samples, NOW), { deltaPct: -18, direction: "down" });
}

{
  // Half a point of movement is inside the ±1 band: a direction would be noise.
  const samples = [
    s(13, 40), s(9, 40),
    s(5, 40.5), s(1, 40.5),
  ];
  assert.deepEqual(weekOverWeek(samples, NOW), { deltaPct: 0.5, direction: "flat" });
  const samplesDown = [
    s(13, 40), s(9, 40),
    s(5, 39.5), s(1, 39.5),
  ];
  assert.deepEqual(weekOverWeek(samplesDown, NOW), { deltaPct: -0.5, direction: "flat" });
}

{
  // A sample exactly on the 7-day boundary belongs to THIS week, so the two halves never
  // share a point. Here that leaves the previous week with a single sample, and one
  // point is not a week: no honest comparison.
  const samples = [s(10, 30), { t: NOW - WEEK, u: 50 }, s(2, 50)];
  assert.equal(weekOverWeek(samples, NOW), null);
}

// Suppression: nothing to compare, so nothing is said.
assert.equal(weekOverWeek([], NOW), null, "no samples");
assert.equal(weekOverWeek(null, NOW), null, "no series");
assert.equal(weekOverWeek(undefined, NOW), null, "missing series");
assert.equal(weekOverWeek([s(1, 40)], NOW), null, "this week only");
assert.equal(weekOverWeek([s(13, 40), s(9, 40)], NOW), null, "last week only");
assert.equal(
  weekOverWeek([s(13, 40), s(1, 50)], NOW),
  null,
  "one sample per half is not a trend",
);
assert.equal(
  weekOverWeek([s(30, 10), s(20, 10)], NOW),
  null,
  "older than the comparison reach",
);
assert.equal(weekOverWeek([s(13, 40), s(9, 40), s(5, 50), s(1, 50)], NaN), null, "no clock");
assert.equal(
  weekOverWeek([s(13, 40), s(9, 40), s(5, 50), s(1, 50)], "now"),
  null,
  "a clock that is not a number",
);

{
  // Garbage points are dropped, not trusted: the finite points still compare cleanly.
  const samples = [
    s(13, 30), { t: "x", u: 9 }, s(9, 30),
    s(5, 48), { u: 48 }, { t: NOW - DAY, u: null }, s(1, 48),
  ];
  assert.deepEqual(weekOverWeek(samples, NOW), { deltaPct: 18, direction: "up" });
}
assert.equal(weekOverWeek([{ t: NaN, u: NaN }], NOW), null, "garbage only");

// --- forecast -----------------------------------------------------------------------

{
  // 20 points a day over the last day, currently at 60: 40 points to go is two days.
  // The answer comes back as epoch milliseconds.
  const samples = [s(1, 40), s(0.5, 50), s(0, 60)];
  assert.equal(forecast(samples, NOW), (NOW + 2 * DAY) * 1000);
}

{
  // Rising, but 2 points a day from 60 needs 20 days to reach 100. Past the 14-day
  // horizon, so it is weather forecasting, not advice.
  const samples = [s(1, 58), s(0.5, 59), s(0, 60)];
  assert.equal(forecast(samples, NOW), null, "rising but too slow to matter");
}

assert.equal(
  forecast([s(1, 50), s(0.5, 50), s(0, 50)], NOW),
  null,
  "flat: nothing runs out",
);
assert.equal(
  forecast([s(1, 60), s(0.5, 55), s(0, 50)], NOW),
  null,
  "falling: the opposite of running out",
);
assert.equal(
  forecast([s(1, 98), s(0.5, 100), s(0, 102)], NOW),
  null,
  "already exhausted: the tile says so",
);

// Suppression: the fit must be honest or it does not exist.
assert.equal(forecast([s(0.5, 50), s(0, 60)], NOW), null, "two points are not a trend");
assert.equal(forecast([s(0, 60)], NOW), null, "one point");
assert.equal(forecast([], NOW), null, "no samples");
assert.equal(forecast(null, NOW), null, "no series");
assert.equal(forecast([s(1, 50), s(0.5, 55), s(0, 60)], NaN), null, "no clock");
assert.equal(
  forecast([{ t: NOW - 600, u: 50 }, { t: NOW - 300, u: 55 }, { t: NOW - 1, u: 60 }], NOW),
  null,
  "a span under an hour is jitter, not a direction",
);
assert.equal(
  forecast([s(10, 10), s(9, 20), s(8, 30)], NOW),
  null,
  "nothing recent to fit",
);

{
  // Garbage points are dropped before the fit; the good points still project.
  const samples = [
    { t: "junk", u: 99 },
    s(1, 40),
    { t: NOW - 0.5 * DAY, u: Infinity },
    s(0.5, 50),
    s(0, 60),
  ];
  assert.equal(forecast(samples, NOW), (NOW + 2 * DAY) * 1000, "garbage is dropped");
}

// --- history lookup -----------------------------------------------------------------

{
  // A provider's samples may live under the bare id and under several account series;
  // they come back merged and in time order.
  const history = {
    claude: [{ t: 3, u: 30 }],
    "claude:aaa": [{ t: 1, u: 10 }],
    "claude:bbb": [{ t: 2, u: 20 }],
    "claude-extra": [{ t: 9, u: 90 }], // a different provider with a similar prefix
    codex: [{ t: 4, u: 40 }],
  };
  assert.deepEqual(
    samplesFor(history, "claude").map((p) => p.t),
    [1, 2, 3],
    "merged across account series, time-ordered",
  );
  assert.deepEqual(samplesFor(history, "codex").map((p) => p.t), [4], "bare key alone");
  assert.deepEqual(samplesFor(history, "nobody"), [], "unknown provider");
  assert.deepEqual(samplesFor(null, "claude"), [], "no history");
  assert.deepEqual(samplesFor({}, "claude"), [], "empty history");
  assert.deepEqual(samplesFor({ claude: "junk" }, "claude"), [], "a non-array series is skipped");
}

console.log("trends.js ok");
