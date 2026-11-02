const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const relative = new Intl.RelativeTimeFormat("en-GB", { numeric: "auto" });

/** "just now", "5 minutes ago", "yesterday", "3 weeks ago". */
export function timeAgo(iso: string, now = Date.now()): string {
  const ms = new Date(iso).getTime() - now;
  const abs = Math.abs(ms);
  if (abs < MINUTE) return "just now";
  if (abs < HOUR) return relative.format(Math.round(ms / MINUTE), "minute");
  if (abs < DAY) return relative.format(Math.round(ms / HOUR), "hour");
  if (abs < 7 * DAY) return relative.format(Math.round(ms / DAY), "day");
  if (abs < 30 * DAY) return relative.format(Math.round(ms / (7 * DAY)), "week");
  return relative.format(Math.round(ms / (30 * DAY)), "month");
}

/** "27 Oct 2026", in the viewer's timezone. */
export function date(iso: string): string {
  return new Date(iso).toLocaleDateString("en-GB", {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

/** "Sun 2 Nov, 14:05", in `timeZone` (a chat's), or the viewer's. */
export function dateTime(iso: string, timeZone?: string): string {
  return new Date(iso).toLocaleString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    timeZone,
  });
}

/** "1 todo", "3 todos". */
export function count(n: number, word: string, plural = `${word}s`): string {
  return `${n} ${n === 1 ? word : plural}`;
}
