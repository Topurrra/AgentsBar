// Percent REMAINING from a UsageWindow, clamped and rounded, or null when the backend
// did not know the number. used_percent is Option<f64> in Rust, so an unknown window
// arrives as JSON null: Number(null) is 0, which would have rendered as a confident
// green "100% left". Only a real, finite number counts.
export function percentLeft(win) {
  const used = win?.used_percent;
  if (typeof used !== "number" || !Number.isFinite(used)) return null;
  return Math.max(0, Math.min(100, Math.round(100 - used)));
}

// green > 50, yellow 20..50, red < 20 (of remaining)
export function tone(left) {
  if (left === null) return "unknown";
  if (left > 50) return "ok";
  if (left >= 20) return "warn";
  return "bad";
}

// "2h 14m" until the reset instant. Ticked locally, no backend call.
export function countdown(iso, now) {
  if (!iso) return "";
  const target = new Date(iso).getTime();
  if (!Number.isFinite(target)) return "";
  const mins = Math.floor((target - now) / 60000);
  if (mins <= 0) return "now";
  const days = Math.floor(mins / 1440);
  const hours = Math.floor((mins % 1440) / 60);
  const rest = mins % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${rest}m`;
  return `${rest}m`;
}

export function clockTime(iso) {
  if (!iso) return "never";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "never";
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function credits(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

// The footer's "refreshed at" is the OLDEST fetched_at, not the newest: see
// oldestFetch in tiles.js. A newest-of helper used to live here and is gone on purpose,
// because the two read identically at a call site and only one of them is honest.
