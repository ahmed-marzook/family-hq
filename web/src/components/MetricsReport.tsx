import ErrorIcon from "@mui/icons-material/Error";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { Link } from "react-router";

import { getJson, type Metrics as MetricsData } from "../api";
import { count, dateTime, timeAgo } from "../format";
import ColumnChart, { type Column } from "./ColumnChart";
import LoadError from "./LoadError";

export type Scope = "all" | "user" | "chat";

const PERIODS = [
  { days: "7", label: "7 days" },
  { days: "30", label: "30 days" },
  { days: "all", label: "All time" },
];

/** The 7 days / 30 days / All time buttons. */
export function PeriodPicker({
  days,
  onChange,
}: {
  days: string;
  onChange: (days: string) => void;
}) {
  return (
    <div role="group" aria-label="Period" className="mb-6 flex gap-1">
      {PERIODS.map((period) => (
        <button
          key={period.label}
          type="button"
          aria-pressed={days === period.days}
          onClick={() => onChange(period.days)}
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
  );
}

/** `/metrics?days=7&user=3`: `filter` is the part after the period. */
export function metricsPath(days: string, filter = ""): string {
  const query = [days === "all" ? "" : `days=${days}`, filter].filter(Boolean).join("&");
  return query ? `/metrics?${query}` : "/metrics";
}

/**
 * One person's or one chat's metrics, with their own period buttons, for
 * the user and group pages. The period isn't kept in the address there.
 */
export function ScopedMetrics({ scope, id }: { scope: "user" | "chat"; id: number }) {
  const [days, setDays] = useState("30");
  const filter = `${scope}=${id}`;
  const { data, error, isPending } = useQuery({
    queryKey: ["metrics", days, filter],
    queryFn: () => getJson<MetricsData>(metricsPath(days, filter)),
  });
  return (
    <>
      <PeriodPicker days={days} onChange={setDays} />
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} />}
      {data && <MetricsReport data={data} scope={scope} />}
    </>
  );
}

/**
 * The metrics: for everyone (`scope` "all"), one person ("user") or one
 * chat ("chat"). Each leaves out what says nothing for it: one person's
 * page has no "people" table, one chat's no "where" table.
 */
export function MetricsReport({ data, scope }: { data: MetricsData; scope: Scope }) {
  const { totals } = data;
  // Inside a user's or group's "Usage" section, the headings go a level down.
  const nested = scope !== "all";
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
        {scope === "user" ? (
          <Stat label="Chats" value={data.chats.length} />
        ) : (
          <Stat label="People" value={totals.people} />
        )}
        <Stat label="Unknown words" value={totals.unknown} />
        <Stat
          label="Failures"
          value={totals.failures}
          alert={totals.failures > 0}
        />
      </div>

      <Section nested={nested} title="Uses per day">
        {totals.uses === 0 ? (
          <Empty>No uses in this period.</Empty>
        ) : (
          <ColumnChart title="Uses per day" columns={dayColumns} unit="uses" />
        )}
      </Section>

      <Section nested={nested} title={`Busiest hours (${data.timezone})`}>
        {totals.uses === 0 ? (
          <Empty>No uses in this period.</Empty>
        ) : (
          <ColumnChart title="Uses by hour of the day" columns={hourColumns} unit="uses" />
        )}
      </Section>

      <Section nested={nested} title="Top commands">
        {data.commands.length === 0 ? (
          <Empty>No commands in this period.</Empty>
        ) : (
          <BarList
            rows={data.commands.map((c) => ({ name: c.name, value: c.uses }))}
          />
        )}
      </Section>

      {scope !== "chat" && (
        <Section nested={nested} title="Where">
          <Table
            head={["Chat", "Uses"]}
            rows={data.chats.map((c) => ({
              key: String(c.id),
              cells: [
                c.kind === "group" ? (
                  <Link key="n" to={`/groups/${c.id}`} className="text-teal-700 hover:underline dark:text-teal-400">
                    {c.name ?? "Unnamed group"}
                  </Link>
                ) : scope === "user" ? (
                  "Private chat"
                ) : (
                  `Private chat: ${c.name ?? "someone"}`
                ),
                c.uses,
              ],
            }))}
            empty="No uses in this period."
            numbers={[1]}
          />
        </Section>
      )}

      {scope !== "user" && (
        <Section nested={nested} title="People">
          <Table
            head={["Name", "Uses", "Active days", "Last seen"]}
            rows={data.people.map((p) => ({
              key: String(p.id),
              muted: p.uses === 0,
              cells: [
                <Link key="n" to={`/users/${p.id}`} className="hover:underline">
                  {p.name}
                </Link>,
                p.uses,
                p.active_days,
                p.last_seen ? timeAgo(p.last_seen) : "Never",
              ],
            }))}
            empty={scope === "chat" ? "Nobody has used the bot here yet." : "Nobody has messaged the bot yet."}
            numbers={[1, 2]}
          />
        </Section>
      )}

      <Section nested={nested} title="Words the bot didn't understand">
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

      <Section nested={nested} title="Failures">
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

      {scope === "all" && (
        <Section nested={nested} title="Slowest commands">
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
      )}
    </>
  );
}

function Section({
  title,
  nested,
  children,
}: {
  title: string;
  nested: boolean;
  children: ReactNode;
}) {
  const Heading = nested ? "h3" : "h2";
  return (
    <section className="mb-8">
      <Heading className={`mb-3 font-semibold ${nested ? "text-base" : "text-lg"}`}>{title}</Heading>
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
