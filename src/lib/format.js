// Percent REMAINING from a UsageWindow, clamped and rounded.
export function percentLeft(win) {
  const used = Number(win?.used_percent);
  if (!Number.isFinite(used)) return null;
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

// Newest fetched_at across snapshots, or null.
export function lastRefresh(snapshots) {
  let best = null;
  for (const s of snapshots) {
    const t = new Date(s?.fetched_at ?? 0).getTime();
    if (Number.isFinite(t) && t > 0 && (best === null || t > best)) best = t;
  }
  return best === null ? null : new Date(best).toISOString();
}
