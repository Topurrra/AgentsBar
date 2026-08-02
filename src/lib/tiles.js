// Display-time decisions for the tile list: which order, which windows, how stale.
// These live outside the components so `node src/lib/tiles.test.js` can prove them
// without a DOM.
import { percentLeft, countdown } from "./format.js";

// The lanes to render, already carrying the row 25 weekly-caps-session clamp.
//
// Row 25 asks for ONE implementation of that clamp, and it lives in Rust
// (`state::display_windows`) because the tray needs it too and the two must never
// disagree. `commands::get_snapshots` and the `usage-updated` event both send the result
// as `windows`, so there is deliberately no second opinion here: this reads the field.
// The raw `primary`/`secondary`/`tertiary` lanes still travel in the payload and are the
// unclamped provider numbers, so do not reach for them to draw a bar.
export const windowsOf = (snap) => snap?.windows ?? [];

// Row 15. Lower sorts first. Exhausted needs no bucket of its own: 0% left is the
// smallest percent left there is. An unknown percentage is NOT zero used, so a
// provider we have no numbers for ranks below every provider we do.
export function urgency(provider, snapshot, pinned) {
  if (provider.id === pinned) return -1;
  if (!provider.configured) return 300;
  if (snapshot?.error) return 400;
  const lefts = windowsOf(snapshot)
    .map(percentLeft)
    .filter((n) => n !== null);
  return lefts.length ? Math.min(...lefts) : 200;
}

// Array.prototype.sort is stable, so equal urgency keeps registry order.
export function sortTiles(providers, byId, pinned) {
  const key = new Map(
    providers.map((p) => [p.id, urgency(p, byId.get(p.id), pinned)]),
  );
  return [...providers].sort((a, b) => key.get(a.id) - key.get(b.id));
}

// Row 20. The footer stamp is the OLDEST fetch across the tiles on screen, not the
// newest: "Updated just now" over an hour-old number is the lie this closes.
export function oldestFetch(snapshots) {
  let best = null;
  for (const s of snapshots) {
    const t = Date.parse(s?.fetched_at ?? "");
    if (Number.isFinite(t) && (best === null || t < best)) best = t;
  }
  return best === null ? null : new Date(best).toISOString();
}

// The footer cost line, aggregated only from money the providers report in USD:
// `spend_usd` is a trailing-window SPEND (OpenAI's last 30 days), and `credits` whose
// `credits_unit` is "USD" is a remaining BALANCE (DeepSeek's USD wallet, OpenRouter,
// xAI, OpenAI credits). A CNY wallet or a token count carries a different unit and is
// deliberately left out, so the total is never dollars plus characters plus tokens.
// Returns null when nothing on screen reports USD, and the caller renders nothing.
export function costSummary(snapshots) {
  let spend = 0;
  let balance = 0;
  let hasSpend = false;
  let hasBalance = false;
  for (const s of snapshots ?? []) {
    if (Number.isFinite(s?.spend_usd)) {
      spend += s.spend_usd;
      hasSpend = true;
    }
    if (s?.credits_unit === "USD" && Number.isFinite(s?.credits)) {
      balance += s.credits;
      hasBalance = true;
    }
  }
  if (!hasSpend && !hasBalance) return null;
  return { spend, balance, hasSpend, hasBalance };
}

// --- row 26: pace ------------------------------------------------------------------

// Below this much of the window elapsed there is no burn rate worth reporting: at 1%
// elapsed a single request reads as "80% in deficit". CodexBar landed on the same 3.
const PACE_MIN_ELAPSED = 3;
// Within this many points of the expected burn you are simply on pace. Their band.
const PACE_BAND = 2;

// Are you ahead of or behind the burn rate for this window, and does what is left reach
// the reset. Ported from CodexBar's `UsagePace.weekly` plus `UsagePaceText`, minus the
// parts this wave does not ask for: no workday weighting, no run-out probability, no
// speed hint.
//
// The suppression rules ARE the feature. A pace line that appears on an empty window, on
// a window we have no number for, or two minutes into a week, teaches the user that the
// line means nothing. Returns null in every one of those cases; the caller renders
// nothing at all rather than a hedge.
export function pace(win, now) {
  const used = win?.used_percent;
  // Unknown is not zero used. Nothing to pace.
  if (typeof used !== "number" || !Number.isFinite(used)) return null;

  const minutes = win?.window_minutes;
  if (typeof minutes !== "number" || !Number.isFinite(minutes) || minutes <= 0) return null;

  const reset = Date.parse(win?.resets_at ?? "");
  if (!Number.isFinite(reset)) return null;

  const duration = minutes * 60000;
  const untilReset = reset - now;
  // Reset already due: the percent on screen belongs to a window that is over.
  if (untilReset <= 0) return null;
  // A reset further out than the window is long means the two fields disagree. Say
  // nothing rather than invent an elapsed fraction out of them.
  if (untilReset > duration) return null;

  const actual = Math.max(0, Math.min(100, used));
  // Exhausted. Pace is a forecast and this one has already happened.
  if (actual >= 100) return null;

  const elapsed = duration - untilReset;
  const expected = (elapsed / duration) * 100;
  if (expected < PACE_MIN_ELAPSED) return null;

  const delta = actual - expected;
  const amount = Math.round(Math.abs(delta));
  const state = Math.abs(delta) <= PACE_BAND ? "even" : delta > 0 ? "deficit" : "reserve";
  const label =
    state === "even"
      ? "On pace"
      : `${amount}% ${state === "deficit" ? "in deficit" : "in reserve"}`;

  // Straight line projection from the burn so far. actual === 0 gives no rate, which is
  // the honest answer that nothing is being consumed, so it lasts.
  const rate = actual / elapsed;
  const eta = rate > 0 ? (100 - actual) / rate : Infinity;
  const lasts = !(eta < untilReset);
  const left = lasts ? "" : countdown(new Date(now + eta).toISOString(), now);

  return {
    state,
    label,
    // The two halves must never argue. A delta anywhere in (0, PACE_BAND] is called "On
    // pace" by the label while the projection undershoots the reset by those same couple
    // of points, which renders "On pace · Runs out in 3d 8h": a line that contradicts
    // itself. Inside the band that half is simply not said. Null, so the caller renders
    // nothing rather than a hedge, exactly as it does for the suppression rules above.
    // Being AHEAD inside the band still says so: that one is both true and reassuring.
    eta: lasts
      ? "Lasts until reset"
      : state === "even"
        ? null
        : left && left !== "now"
          ? `Runs out in ${left}`
          : "Runs out now",
  };
}

// --- rows 21 and 22: error copy ------------------------------------------------------

// The "where" half of the template. doc_url is the provider's own page, which is where
// the user signs in or picks up a key.
function hostOf(url) {
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return "";
  }
}

// One template for every provider error: what failed, what to do, where. A raw
// "HTTP 401" tells the user nothing they can act on.
//
// `tone` is the row 21 UI half: "bad" is an auth failure, red, needs the user; "warn" is
// a setup gap; "muted" is transient and self-healing, so it must not read as an alarm.
// Switches on `error_kind` from the snapshot, never on the message text.
export function errorCopy(provider, snapshot, cookieSource) {
  if (!snapshot?.error) return null;

  const name = provider?.name ?? "This provider";
  const where = hostOf(provider?.doc_url);
  const at = where ? ` at ${where}` : "";
  const auth = provider?.auth;

  // The self-caused case, and the only one Retry cannot fix.
  if (auth === "cookie" && cookieSource === "off") {
    return {
      tone: "warn",
      retry: false,
      text: "Cookie import is off for this provider. Turn it on in Settings.",
    };
  }

  // A null kind next to a non-null error is a payload from a build before the taxonomy
  // landed. Transient is the safe read: it never colours a tile red on a guess.
  switch (snapshot.error_kind ?? "http") {
    case "auth":
      if (auth === "cookie")
        return {
          tone: "bad",
          retry: true,
          text: `Your ${name} session expired. Sign in${at} in your browser, then Retry.`,
        };
      if (auth === "api_key" || auth === "token") {
        // Row 38. The key may have come from the environment, in which case Settings shows
        // an empty field and pasting there changes nothing about the credential in use. So
        // name the variable when the provider reads one.
        const or = provider?.env_key
          ? `, or in ${provider.env_key} if you set it there`
          : "";
        return {
          tone: "bad",
          retry: true,
          text: `${name} rejected the saved key. Get a new one${at} and paste it in Settings${or}.`,
        };
      }
      if (auth === "oauth_file")
        return {
          tone: "bad",
          retry: true,
          text: `${name} sign-in expired. Sign in again with the ${name} CLI, then Retry.`,
        };
      return {
        tone: "bad",
        retry: true,
        text: `${name} rejected the request. Check your account${at}, then Retry.`,
      };

    case "not_configured":
      if (auth === "cookie")
        return {
          tone: "warn",
          retry: true,
          text: `No ${name} session found. Sign in${at} in your browser, or paste a session in Settings.`,
        };
      if (auth === "api_key" || auth === "token")
        return {
          tone: "warn",
          retry: true,
          text: `No API key for ${name}. Get one${at} and paste it in Settings.`,
        };
      if (auth === "oauth_file")
        return {
          tone: "warn",
          retry: true,
          text: `Not signed in to ${name}. Sign in with the ${name} CLI, then Retry.`,
        };
      return { tone: "warn", retry: false, text: `${name} is not set up. Open Settings to finish.` };

    case "rate_limited":
      // Retry stays: a manual refresh deliberately bypasses the backoff.
      return {
        tone: "muted",
        retry: true,
        text: `${name} is rate limiting us. Waiting before the next check.`,
      };

    case "parse":
      return {
        tone: "muted",
        retry: true,
        text: `${name} returned usage we could not read. The next refresh usually clears it.`,
      };

    default:
      return {
        tone: "muted",
        retry: true,
        text: `Could not reach ${name}. Retrying on the next refresh.`,
      };
  }
}

// "4m ago", shown per tile only once that tile is past twice the refresh interval.
export function ago(iso, now) {
  const t = Date.parse(iso ?? "");
  if (!Number.isFinite(t)) return "";
  const mins = Math.floor((now - t) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}
