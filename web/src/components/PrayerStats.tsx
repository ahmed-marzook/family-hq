import LocalFireDepartmentIcon from "@mui/icons-material/LocalFireDepartment";
import { Link } from "react-router";

import type { PrayerSpan } from "../api";

/** The day's square, in the same colours as the WhatsApp reports' emoji. */
const levels: { test: (prayed: number | null) => boolean; className: string; label: string }[] = [
  { test: (p) => p === 5, className: "bg-green-500", label: "all 5" },
  { test: (p) => p === 3 || p === 4, className: "bg-yellow-400", label: "3–4" },
  { test: (p) => p === 1 || p === 2, className: "bg-orange-500", label: "1–2" },
  { test: (p) => p === 0, className: "bg-red-500", label: "none" },
  {
    test: (p) => p === null,
    className: "bg-stone-200 dark:bg-stone-700",
    label: "no record",
  },
];

function levelOf(prayed: number | null) {
  return levels.find((l) => l.test(prayed)) ?? levels[levels.length - 1];
}

/** "2026-10-27" → "Tue 27 Oct". Read as UTC, so no timezone shifts the day. */
function dayName(day: string): string {
  return new Date(`${day}T00:00:00Z`).toLocaleDateString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    timeZone: "UTC",
  });
}

/** Monday = 0 … Sunday = 6, as the bot's weeks run. */
function weekdayFromMonday(day: string): number {
  return (new Date(`${day}T00:00:00Z`).getUTCDay() + 6) % 7;
}

function Square({ day, prayed }: { day: string; prayed: number | null }) {
  const text = `${dayName(day)}: ${prayed === null ? "no record" : `${prayed}/5 prayed`}`;
  return (
    <span
      title={text}
      aria-label={text}
      role="img"
      className={`block size-5 rounded-sm ${levelOf(prayed).className}`}
    />
  );
}

/**
 * A week as one row of seven squares, or a month as a calendar (Monday
 * first), like the WhatsApp reports.
 */
export function Heatmap({ span, calendar = false }: { span: PrayerSpan; calendar?: boolean }) {
  const before = calendar && span.days.length > 0 ? weekdayFromMonday(span.days[0].date) : 0;
  return (
    <div className="grid w-fit grid-cols-7 gap-1">
      {Array.from({ length: before }, (_, i) => (
        <span key={`pad-${i}`} aria-hidden className="block size-5" />
      ))}
      {span.days.map((d) => (
        <Square key={d.date} day={d.date} prayed={d.prayed} />
      ))}
    </div>
  );
}

export function HeatmapKey() {
  return (
    <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-stone-500">
      {levels.map((l) => (
        <span key={l.label} className="inline-flex items-center gap-1">
          <span aria-hidden className={`inline-block size-3 rounded-sm ${l.className}`} />
          {l.label}
        </span>
      ))}
    </p>
  );
}

/** "Fajr ×2, Isha". */
function breakdown(counts: { prayer: string; count: number }[]): string {
  return counts.map((c) => (c.count > 1 ? `${c.prayer} ×${c.count}` : c.prayer)).join(", ");
}

function Totals({ span }: { span: PrayerSpan }) {
  return (
    <>
      <p className="text-sm">
        <span className="font-medium">{span.percent}%</span>{" "}
        <span className="text-stone-600 dark:text-stone-400">
          ({span.prayed}/{span.recorded} prayed)
        </span>
        <span className="ml-3 inline-flex items-center gap-0.5 text-stone-600 dark:text-stone-400">
          <LocalFireDepartmentIcon className="size-4 text-orange-500" />
          Longest streak: {span.longest_streak}
        </span>
      </p>
      <p className="text-sm text-stone-600 dark:text-stone-400">
        Missed: {span.missed.length ? breakdown(span.missed) : "none"}
        {span.made_up.length > 0 && <> · Made up: {breakdown(span.made_up)}</>}
      </p>
    </>
  );
}

const card =
  "rounded-lg border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900";

/** One week or month: the heatmap, completion, streak and what was missed. */
export function PrayerSummary({
  title,
  span,
  calendar = false,
}: {
  title: string;
  span: PrayerSpan;
  calendar?: boolean;
}) {
  return (
    <div className={card}>
      <h3 className="mb-2 font-medium">
        {title}{" "}
        <span className="text-sm font-normal text-stone-500">
          {dayName(span.first)} – {dayName(span.last)}
        </span>
      </h3>
      {span.recorded === 0 ? (
        <p className="text-sm text-stone-600 dark:text-stone-400">Nothing recorded yet.</p>
      ) : (
        <div className="flex flex-col gap-2">
          <Heatmap span={span} calendar={calendar} />
          <Totals span={span} />
        </div>
      )}
    </div>
  );
}

/** A group's members, each with their week so far. */
export function MemberPrayers({
  members,
}: {
  members: { id: number; name: string; prayer_week: PrayerSpan }[];
}) {
  const tracked = members.filter((m) => m.prayer_week.recorded > 0);
  if (tracked.length === 0) {
    return (
      <p className="text-sm text-stone-600 dark:text-stone-400">
        Nothing recorded this week. Prayers are tracked once reminders are on.
      </p>
    );
  }
  return (
    <div className="overflow-x-auto">
      <table className="text-sm">
        <thead>
          <tr className="text-left text-xs text-stone-500">
            <th className="py-1 pr-4 font-medium">Member</th>
            <th className="py-1 pr-4 font-medium">Mon → Sun</th>
            <th className="py-1 pr-4 font-medium">Prayed</th>
            <th className="py-1 pr-4 font-medium">Streak</th>
            <th className="py-1 font-medium">Missed</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-stone-200 dark:divide-stone-800">
          {tracked.map((m) => (
            <tr key={m.id}>
              <td className="py-2 pr-4">
                <Link to={`/users/${m.id}`} className="hover:underline">
                  {m.name}
                </Link>
              </td>
              <td className="py-2 pr-4">
                <Heatmap span={m.prayer_week} />
              </td>
              <td className="py-2 pr-4 whitespace-nowrap">{m.prayer_week.percent}%</td>
              <td className="py-2 pr-4">{m.prayer_week.longest_streak}</td>
              <td className="py-2 text-stone-600 dark:text-stone-400">
                {m.prayer_week.missed.length ? breakdown(m.prayer_week.missed) : "none"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
