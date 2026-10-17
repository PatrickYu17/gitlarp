export type Counts = Record<string, number>;

// UTC everywhere: the API validates and clamps dates against UTC, so
// local-time cells would drift a day at boundaries.
const pad2 = (n: number) => String(n).padStart(2, "0");

export const fmt = (d: Date) =>
  `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;

/** Clamp the plan's 0–4 commit levels (matches the cell shading). */
export const clampLevel = (v: number) => Math.max(0, Math.min(4, Math.floor(v) || 0));

export function lastYear(): string[] {
  const days: string[] = [];
  for (let i = 364; i >= 0; i--) {
    const d = new Date();
    d.setUTCHours(12, 0, 0, 0);
    d.setUTCDate(d.getUTCDate() - i);
    days.push(fmt(d));
  }
  return days;
}

/** Weekday of the first cell, so the grid's rows line up Sun..Sat. */
export function leadingPad(): number {
  const start = new Date();
  start.setUTCHours(12, 0, 0, 0);
  start.setUTCDate(start.getUTCDate() - 364);
  return start.getUTCDay();
}

export const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/**
 * "Tue, Mar 3, 2026" for a YYYY-MM-DD day. Hand-rolled rather than
 * toLocaleDateString so server and browser render identical text.
 */
export function prettyDay(day: string, weekday = false): string {
  const d = new Date(day + "T12:00:00Z");
  const s = `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()}`;
  return weekday ? `${WEEKDAYS[d.getUTCDay()]}, ${s}` : s;
}

/** Month labels for the graph's week columns, skipping cramped ones. */
export function monthLabels(days: string[], pad: number): { col: number; label: string }[] {
  const out: { col: number; label: string }[] = [];
  days.forEach((d, i) => {
    if (i !== 0 && !d.endsWith("-01")) return;
    const col = Math.floor((pad + i) / 7);
    // a label needs ~3 columns of room; a partial first month yields its slot
    if (out.length && col - out[out.length - 1].col < 3) out.pop();
    out.push({ col, label: MONTHS[+d.slice(5, 7) - 1] });
  });
  return out;
}
