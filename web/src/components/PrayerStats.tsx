import ChevronLeftIcon from "@mui/icons-material/ChevronLeft";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import LocalFireDepartmentIcon from "@mui/icons-material/LocalFireDepartment";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";

import {
  ApiError,
  getJson,
  sendJson,
  type PrayerDay,
  type PrayerMonth,
  type PrayerName,
  type PrayerSpan,
  type PrayerStatus,
} from "../api";

/** The day's square, in the same colours as the WhatsApp reports' emoji. */
const levels: {
  test: (prayed: number | null) => boolean;
  className: string;
  label: string;
}[] = [
  { test: (p) => p === 5, className: "bg-green-500 text-white", label: "all 5" },
  { test: (p) => p === 3 || p === 4, className: "bg-yellow-400 text-stone-900", label: "3–4" },
  { test: (p) => p === 1 || p === 2, className: "bg-orange-500 text-white", label: "1–2" },
  { test: (p) => p === 0, className: "bg-red-500 text-white", label: "none" },
  {
    test: (p) => p === null,
    className: "bg-stone-200 text-stone-500 dark:bg-stone-700 dark:text-stone-400",
    label: "no record",
  },
];

function levelOf(prayed: number | null) {
  return levels.find((l) => l.test(prayed)) ?? levels[levels.length - 1];
}

/** How each status reads, with the WhatsApp mark. */
const statuses: { value: PrayerStatus; mark: string; label: string }[] = [
  { value: "prayed", mark: "✅", label: "Prayed" },
  { value: "late", mark: "🕐", label: "Made up" },
  { value: "missed", mark: "❌", label: "Missed" },
  { value: null, mark: "⬜", label: "No record" },
];

const statusOf = (value: PrayerStatus) => statuses.find((s) => s.value === value)!;

/** "fajr" → "Fajr". */
const prayerName = (p: PrayerName) => p[0].toUpperCase() + p.slice(1);

/** A "2026-10-27" date, read as UTC so no timezone shifts the day. */
const utcDate = (day: string) => new Date(`${day}T00:00:00Z`);

/** "2026-10-27" → "Tue 27 Oct". */
function dayName(day: string): string {
  return utcDate(day).toLocaleDateString("en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    timeZone: "UTC",
  });
}

/** Monday = 0 … Sunday = 6, as the bot's weeks run. */
const weekdayFromMonday = (day: string) => (utcDate(day).getUTCDay() + 6) % 7;

/** "2026-11-02" → "2026-11"; and one month on or back. */
function monthKey(day: string, step = 0): string {
  const d = utcDate(day);
  d.setUTCDate(1);
  d.setUTCMonth(d.getUTCMonth() + step);
  return d.toISOString().slice(0, 7);
}

/** What hovering a day shows: its date and each prayer. */
function DayDetail({ day }: { day: PrayerDay }) {
  return (
    <span
      role="tooltip"
      className="pointer-events-none absolute bottom-full left-1/2 z-20 mb-1.5 hidden w-36 -translate-x-1/2 rounded-md border border-stone-200 bg-white p-2 text-left text-xs font-normal text-stone-800 shadow-lg group-hover:block group-focus-visible:block dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
    >
      <span className="mb-1 block font-medium">{dayName(day.date)}</span>
      {day.prayers.map((p) => {
        const s = statusOf(p.status);
        return (
          <span key={p.prayer} className="flex justify-between gap-2">
            <span>{prayerName(p.prayer)}</span>
            <span>
              {s.mark} {s.label}
            </span>
          </span>
        );
      })}
    </span>
  );
}

/**
 * One day's square: coloured by how many were prayed, with the day of the
 * month inside and its prayers on hover. Clickable when `onSelect` is set.
 */
function Square({
  day,
  today,
  selected = false,
  onSelect,
}: {
  day: PrayerDay;
  today?: string;
  selected?: boolean;
  onSelect?: (date: string) => void;
}) {
  const future = today !== undefined && day.date > today;
  const label = `${dayName(day.date)}: ${
    day.prayed === null ? "no record" : `${day.prayed}/5 prayed`
  }`;
  const ring = selected
    ? "ring-2 ring-teal-600 ring-offset-1 dark:ring-offset-stone-900"
    : day.date === today
      ? "ring-2 ring-stone-500 ring-offset-1 dark:ring-stone-300 dark:ring-offset-stone-900"
      : "";
  const className = `group relative flex size-8 items-center justify-center rounded-md text-xs font-medium tabular-nums focus-visible:outline-2 focus-visible:outline-teal-600 ${
    levelOf(day.prayed).className
  } ${ring} ${future ? "opacity-40" : ""}`;
  const number = utcDate(day.date).getUTCDate();

  if (future) {
    return (
      <span aria-label={`${dayName(day.date)}: still to come`} className={className}>
        {number}
      </span>
    );
  }
  // Not clickable: a plain square, still focusable so the prayers show.
  if (!onSelect) {
    return (
      <span tabIndex={0} aria-label={label} className={className}>
        {number}
        <DayDetail day={day} />
      </span>
    );
  }
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={selected}
      onClick={() => onSelect(day.date)}
      className={`${className} cursor-pointer hover:brightness-110`}
    >
      {number}
      <DayDetail day={day} />
    </button>
  );
}

const WEEKDAYS = ["M", "T", "W", "T", "F", "S", "S"];

/**
 * A week as one row of seven squares, or a month as a calendar (Monday
 * first), like the WhatsApp reports.
 */
export function Heatmap({
  span,
  calendar = false,
  today,
  selected,
  onSelect,
}: {
  span: PrayerSpan;
  calendar?: boolean;
  today?: string;
  selected?: string;
  onSelect?: (date: string) => void;
}) {
  const before = calendar && span.days.length > 0 ? weekdayFromMonday(span.days[0].date) : 0;
  return (
    <div className="grid w-fit grid-cols-7 gap-1.5">
      {calendar &&
        WEEKDAYS.map((d, i) => (
          <span key={i} aria-hidden className="text-center text-xs text-stone-500">
            {d}
          </span>
        ))}
      {Array.from({ length: before }, (_, i) => (
        <span key={`pad-${i}`} aria-hidden className="block size-8" />
      ))}
      {span.days.map((d) => (
        <Square
          key={d.date}
          day={d}
          today={today}
          selected={d.date === selected}
          onSelect={onSelect}
        />
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
      <span>· hover a day to see each prayer</span>
    </p>
  );
}

/** "Fajr ×2, Isha". */
function breakdown(counts: { prayer: string; count: number }[]): string {
  return counts.map((c) => (c.count > 1 ? `${c.prayer} ×${c.count}` : c.prayer)).join(", ");
}

function Totals({ span }: { span: PrayerSpan }) {
  if (span.recorded === 0) {
    return <p className="text-sm text-stone-600 dark:text-stone-400">Nothing recorded.</p>;
  }
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

/** One week: the heatmap, completion, streak and what was missed. */
export function PrayerSummary({ title, span }: { title: string; span: PrayerSpan }) {
  return (
    <div className={card}>
      <h3 className="mb-2 font-medium">
        {title}{" "}
        <span className="text-sm font-normal text-stone-500">
          {dayName(span.first)} – {dayName(span.last)}
        </span>
      </h3>
      <div className="flex flex-col gap-2">
        <Heatmap span={span} />
        <Totals span={span} />
      </div>
    </div>
  );
}

/**
 * Any month of one person's prayers, as a calendar. Click a day (today to
 * start with) to see and correct its five prayers.
 */
export function PrayerCalendar({ userId }: { userId: number }) {
  // `null` until the first answer says what this month is for them.
  const [month, setMonth] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const queryKey = ["prayers", String(userId), month ?? "now"];
  const { data, error, isPending } = useQuery({
    queryKey,
    queryFn: () =>
      getJson<PrayerMonth>(`/users/${userId}/prayers${month ? `?month=${month}` : ""}`),
  });

  if (isPending) return <p className="text-sm text-stone-500">Loading…</p>;
  if (error || !data) {
    return (
      <p role="alert" className="text-sm text-red-700 dark:text-red-400">
        Couldn't load their prayers.
      </p>
    );
  }

  const shown = data.month;
  const current = monthKey(shown.first);
  const isThisMonth = current === monthKey(data.today);
  const day =
    shown.days.find((d) => d.date === selected) ??
    shown.days.find((d) => d.date === data.today) ??
    null;
  const go = (step: number) => {
    setMonth(monthKey(shown.first, step));
    setSelected(null);
  };
  const title = utcDate(shown.first).toLocaleDateString("en-GB", {
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  });

  return (
    <div className={card}>
      <div className="mb-3 flex items-center gap-2">
        <button
          type="button"
          onClick={() => go(-1)}
          aria-label="Previous month"
          className="rounded p-1 text-stone-500 hover:bg-stone-100 hover:text-stone-800 dark:hover:bg-stone-800 dark:hover:text-stone-200"
        >
          <ChevronLeftIcon className="size-5" />
        </button>
        <h3 className="min-w-36 text-center font-medium">{title}</h3>
        <button
          type="button"
          onClick={() => go(1)}
          disabled={isThisMonth}
          aria-label="Next month"
          className="rounded p-1 text-stone-500 hover:bg-stone-100 hover:text-stone-800 disabled:opacity-30 dark:hover:bg-stone-800 dark:hover:text-stone-200"
        >
          <ChevronRightIcon className="size-5" />
        </button>
        {!isThisMonth && (
          <button
            type="button"
            onClick={() => {
              setMonth(null);
              setSelected(null);
            }}
            className="ml-1 text-sm text-teal-700 hover:underline dark:text-teal-400"
          >
            This month
          </button>
        )}
      </div>
      <div className="flex flex-wrap gap-6">
        <div className="flex flex-col gap-2">
          <Heatmap
            span={shown}
            calendar
            today={data.today}
            selected={day?.date}
            onSelect={setSelected}
          />
          <Totals span={shown} />
        </div>
        {day ? (
          <DayEditor
            userId={userId}
            day={day}
            isToday={day.date === data.today}
            queryKey={queryKey}
          />
        ) : (
          <p className="text-sm text-stone-600 dark:text-stone-400">
            Click a day to see or change its prayers.
          </p>
        )}
      </div>
    </div>
  );
}

/** One day's five prayers, each changeable. Not announced to the person. */
function DayEditor({
  userId,
  day,
  isToday,
  queryKey,
}: {
  userId: number;
  day: PrayerDay;
  isToday: boolean;
  queryKey: string[];
}) {
  const queryClient = useQueryClient();
  const change = useMutation({
    mutationFn: ({ prayer, status }: { prayer: PrayerName; status: PrayerStatus }) =>
      sendJson<PrayerMonth>("PUT", `/users/${userId}/prayers/${day.date}/${prayer}`, {
        status,
      }),
    onSuccess: (data) => {
      // The answer is the month, updated; the week card is refetched.
      queryClient.setQueryData(queryKey, data);
      queryClient.invalidateQueries({ queryKey: ["user", String(userId)] });
    },
  });

  return (
    <div className="min-w-0 flex-1">
      <h4 className="mb-2 text-sm font-medium">
        {isToday ? "Today" : dayName(day.date)}
        {isToday && (
          <span className="ml-1 font-normal text-stone-500">({dayName(day.date)})</span>
        )}
      </h4>
      <fieldset disabled={change.isPending} className="flex flex-col gap-2">
        <legend className="sr-only">Prayers on {dayName(day.date)}</legend>
        {day.prayers.map((p) => (
          <div key={p.prayer} className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <span className="w-16 text-sm">{prayerName(p.prayer)}</span>
            <div
              role="radiogroup"
              aria-label={prayerName(p.prayer)}
              className="inline-flex rounded-md border border-stone-300 dark:border-stone-700"
            >
              {statuses.map((s) => {
                const on = s.value === p.status;
                return (
                  <button
                    key={s.label}
                    type="button"
                    role="radio"
                    aria-checked={on}
                    title={s.label}
                    onClick={() => !on && change.mutate({ prayer: p.prayer, status: s.value })}
                    className={`px-2 py-1 text-xs first:rounded-l-md last:rounded-r-md focus-visible:outline-2 focus-visible:outline-teal-600 disabled:opacity-50 ${
                      on
                        ? "bg-teal-600 text-white"
                        : "bg-white hover:bg-stone-50 dark:bg-stone-900 dark:hover:bg-stone-800"
                    }`}
                  >
                    <span aria-hidden>{s.mark}</span>{" "}
                    <span className="hidden sm:inline">{s.label}</span>
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </fieldset>
      {change.error && (
        <p role="alert" className="mt-2 text-sm text-red-700 dark:text-red-400">
          {change.error instanceof ApiError && change.error.reason
            ? change.error.reason
            : "Couldn't save that."}
        </p>
      )}
      <p className="mt-2 text-xs text-stone-500">
        Changes save straight away; the bot doesn't tell them.
      </p>
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
    // No `overflow-x-auto` wrapper: it would clip each day's hover detail.
    // On a phone the streak and missed columns are left out instead.
    <div>
      <table className="text-sm">
        <thead>
          <tr className="text-left text-xs text-stone-500">
            <th className="py-1 pr-4 font-medium">Member</th>
            <th className="py-1 pr-4 font-medium">Mon → Sun</th>
            <th className="py-1 pr-4 font-medium">Prayed</th>
            <th className="hidden py-1 pr-4 font-medium sm:table-cell">Streak</th>
            <th className="hidden py-1 font-medium sm:table-cell">Missed</th>
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
              <td className="hidden py-2 pr-4 sm:table-cell">{m.prayer_week.longest_streak}</td>
              <td className="hidden py-2 text-stone-600 sm:table-cell dark:text-stone-400">
                {m.prayer_week.missed.length ? breakdown(m.prayer_week.missed) : "none"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
