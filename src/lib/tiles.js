// Display-time decisions for the tile list: which order, which windows, how stale.
// These live outside the components so `node src/lib/tiles.test.js` can prove them
// without a DOM.
import { percentLeft } from "./format.js";

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
