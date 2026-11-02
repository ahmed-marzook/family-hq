import ErrorIcon from "@mui/icons-material/Error";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { useSearchParams } from "react-router";

import { getJson, type Metrics as MetricsData } from "../api";
import ColumnChart, { type Column } from "../components/ColumnChart";
import LoadError from "../components/LoadError";
import Page from "../components/Page";
import { count, dateTime, timeAgo } from "../format";

const PERIODS = [
  { days: "7", label: "7 days" },
  { days: "30", label: "30 days" },
  { days: "all", label: "All time" },
];

export default function Metrics() {
  // The period lives in the address (`?days=7`), so a reload or a shared
  // link shows the same view. No `days` means 30.
  const [params, setParams] = useSearchParams();
  const days = params.get("days") ?? "30";

  const { data, error, isPending } = useQuery({
    queryKey: ["metrics", days],
    queryFn: () =>
      getJson<MetricsData>(days === "all" ? "/metrics" : `/metrics?days=${days}`),
  });

  return (
    <Page title="Metrics">
      <div role="group" aria-label="Period" className="mb-6 flex gap-1">
        {PERIODS.map((period) => (
          <button
            key={period.label}
            type="button"
            aria-pressed={days === period.days}
            onClick={() => setParams(period.days === "30" ? {} : { days: period.days })}
            className={`rounded-md px-3 py-1.5 text-sm font-medium ${
              days === period.days
                ? "bg-teal-50 text-teal-800 dark:bg-teal-950 dark:text-teal-300"
                : "text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
            }`}
          >
            {period.label}
          </button>
        ))}
      </div>
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} />}
      {data && <Report data={data} />}
    </Page>
  );
}

function Report({ data }: { data: MetricsData }) {
  const { totals } = data;
  const hourNames = (h: number) => `${String(h).padStart(2, "0")}:00`;

  const dayColumns: Column[] = data.per_day.map((d, i) => {
    const when = new Date(`${d.day}T12:00:00Z`);
    const label = when.toLocaleDateString("en-GB", {
      weekday: "short",
      day: "numeric",
      month: "short",
      timeZone: "UTC",
    });
    // About six date labels along the axis, starting with the first day.
    const every = Math.max(1, Math.ceil(data.per_day.length / 6));
    const tick =
      i % every === 0
        ? when.toLocaleDateString("en-GB", { day: "numeric", month: "short", timeZone: "UTC" })
        : undefined;
    return { value: d.uses, tick, label: `${label}, ${count(d.people, "person", "people")}` };
  });

  const hourColumns: Column[] = data.hours.map((uses, h) => ({
    value: uses,
    tick: h % 6 === 0 ? hourNames(h) : undefined,
    label: `${hourNames(h)} to ${hourNames((h + 1) % 24)}`,
  }));

  return (
    <>
      <div className="mb-8 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Stat label="Uses" value={totals.uses} />
        <Stat label="People" value={totals.people} />
        <Stat label="Unknown words" value={totals.unknown} />
        <Stat
          label="Failures"
          value={totals.failures}
          alert={totals.failures > 0}
        />
      </div>

      <Section title="Uses per day">
        {totals.uses === 0 ? (
          <Empty>No uses in this period.</Empty>
        ) : (
          <ColumnChart title="Uses per day" columns={dayColumns} unit="uses" />
        )}
      </Section>

      <Section title={`Busiest hours (${data.timezone})`}>
        {totals.uses === 0 ? (
          <Empty>No uses in this period.</Empty>
        ) : (
          <ColumnChart title="Uses by hour of the day" columns={hourColumns} unit="uses" />
        )}
      </Section>

      <Section title="Top commands">
        {data.commands.length === 0 ? (
          <Empty>No commands in this period.</Empty>
        ) : (
          <BarList
            rows={data.commands.map((c) => ({ name: c.name, value: c.uses }))}
          />
        )}
      </Section>

      <Section title="People">
        <Table
          head={["Name", "Uses", "Active days", "Last seen"]}
          rows={data.people.map((p) => ({
            key: p.name,
            muted: p.uses === 0,
            cells: [
              p.name,
              p.uses,
              p.active_days,
              p.last_seen ? timeAgo(p.last_seen) : "Never",
            ],
          }))}
          empty="Nobody has messaged the bot yet."
          numbers={[1, 2]}
        />
      </Section>

      <Section title="Words the bot didn't understand">
        <Table
          head={["Typed", "Times", "People"]}
          rows={data.unknown.map((u) => ({
            key: u.typed,
            cells: [<code key="t">{u.typed}</code>, u.times, u.people],
          }))}
          empty="Nothing unrecognised in this period."
          numbers={[1, 2]}
        />
      </Section>

      <Section title="Failures">
        {data.failures.length > 0 && (
          <p className="mb-2 text-sm text-stone-600 dark:text-stone-400">
            The error itself is in the server log at that time.
            {totals.failures > data.failures.length &&
              ` The latest ${data.failures.length} of ${totals.failures}.`}
          </p>
        )}
        <Table
          head={["When", "Who", "Command", "Detail"]}
          rows={data.failures.map((f, i) => ({
            key: String(i),
            cells: [
              dateTime(f.at, data.timezone),
              f.person,
              <code key="n">{f.name}</code>,
              f.detail ?? "",
            ],
          }))}
          empty="No failures in this period."
          numbers={[]}
        />
      </Section>

      <Section title="Slowest commands">
        <Table
          head={["Command", "Uses", "Average", "Slowest"]}
          rows={data.slowest.map((s) => ({
            key: s.name,
            cells: [<code key="n">{s.name}</code>, s.uses, `${s.avg_ms} ms`, `${s.max_ms} ms`],
          }))}
          empty="No commands in this period."
          numbers={[1, 2, 3]}
        />
      </Section>
    </>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mb-8">
      <h2 className="mb-3 text-lg font-semibold">{title}</h2>
      {children}
    </section>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="text-sm text-stone-600 dark:text-stone-400">{children}</p>;
}

function Stat({ label, value, alert = false }: { label: string; value: number; alert?: boolean }) {
  return (
    <div className="rounded-lg border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-1.5 text-sm text-stone-600 dark:text-stone-400">
        {alert && <ErrorIcon className="size-4 text-red-700 dark:text-red-400" aria-hidden />}
        {label}
      </div>
      <div className="mt-1 text-2xl font-semibold tabular-nums">{value}</div>
    </div>
  );
}

/** Horizontal bars, one per row, longest first; the number in plain ink. */
function BarList({ rows }: { rows: { name: string; value: number }[] }) {
  const max = Math.max(1, ...rows.map((r) => r.value));
  return (
    <ul className="flex flex-col gap-2">
      {rows.map((row) => (
        <li key={row.name} className="grid grid-cols-[minmax(0,10rem)_1fr_3rem] items-center gap-3 text-sm">
          <code className="truncate" title={row.name}>
            {row.name}
          </code>
          <div className="h-3">
            <div
              className="h-full rounded-r bg-teal-600"
              style={{ width: `${(row.value / max) * 100}%`, minWidth: 2 }}
            />
          </div>
          <span className="text-right tabular-nums">{row.value}</span>
        </li>
      ))}
    </ul>
  );
}

type Row = { key: string; cells: ReactNode[]; muted?: boolean };

/** A table; `numbers` are the columns (by position) that hold numbers, which
 * line up on the right. */
function Table({
  head,
  rows,
  empty,
  numbers,
}: {
  head: string[];
  rows: Row[];
  empty: string;
  numbers: number[];
}) {
  const align = (i: number) => (numbers.includes(i) ? "text-right" : "");
  if (rows.length === 0) return <Empty>{empty}</Empty>;
  return (
    <div className="overflow-x-auto rounded-lg border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-stone-200 text-left text-stone-600 dark:border-stone-800 dark:text-stone-400">
            {head.map((h, i) => (
              <th key={h} className={`px-3 py-2 font-medium ${align(i)}`}>
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="divide-y divide-stone-100 tabular-nums dark:divide-stone-800">
          {rows.map((row) => (
            <tr key={row.key} className={row.muted ? "text-stone-500" : ""}>
              {row.cells.map((cell, i) => (
                <td key={i} className={`px-3 py-2 ${align(i)}`}>
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
