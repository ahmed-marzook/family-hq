import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import GroupsIcon from "@mui/icons-material/Groups";
import LockIcon from "@mui/icons-material/Lock";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Link, useParams } from "react-router";

import { getJson, type Chat, type Setting, type UserDetail } from "../api";
import GroupLists from "../components/GroupLists";
import LoadError from "../components/LoadError";
import Page from "../components/Page";
import { count, date, timeAgo } from "../format";

export default function User() {
  const { id } = useParams();
  const { data, error, isPending } = useQuery({
    queryKey: ["user", id],
    queryFn: () => getJson<UserDetail>(`/users/${id}`),
  });

  return (
    <>
      <Link
        to="/users"
        className="mb-4 inline-flex items-center gap-1 text-sm text-teal-700 hover:underline dark:text-teal-400"
      >
        <ArrowBackIcon className="size-4" />
        Users
      </Link>
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} notFound="There's no such user." />}
      {data && <Detail data={data} />}
    </>
  );
}

function Detail({ data }: { data: UserDetail }) {
  const { user, settings } = data;
  const dm = data.chats.find((c) => c.kind === "dm");
  const groups = data.chats.filter((c) => c.kind === "group");

  return (
    <Page title={user.name}>
      <p className="mb-6 text-sm text-stone-600 dark:text-stone-400">
        First seen {date(user.first_seen)} ·{" "}
        {user.last_seen ? `last seen ${timeAgo(user.last_seen)}` : "never used a command"} ·{" "}
        {count(user.recent_uses, "use")} in the last {data.recent_days} days
      </p>

      <Section title="Settings">
        <dl className="divide-y divide-stone-200 dark:divide-stone-800">
          <SettingRow label="Town" setting={settings.place} />
          <SettingRow label="Timezone" setting={settings.timezone} />
          <SettingRow label="Prayer method" setting={settings.prayer_method} />
          <SettingRow label="Asr" setting={settings.asr_school} />
          <SettingRow
            label="“Ending soon” reminder"
            setting={{
              value: `${settings.ending_minutes.value} minutes before`,
              own: settings.ending_minutes.own,
            }}
          />
          <Row label="Prayer reminders (private chat)">
            {settings.prayer_reminders ? "On" : "Off"}
          </Row>
        </dl>
        {!dm && (
          <p className="mt-3 text-sm text-stone-600 dark:text-stone-400">
            They haven't messaged the bot privately, so these are all the
            defaults.
          </p>
        )}
      </Section>

      <Section title="Chats">
        <div className="flex flex-col gap-3">
          {dm && <DmCard chat={dm} />}
          {groups.map((group) => (
            <GroupCard key={group.id} chat={group} />
          ))}
          {!dm && groups.length === 0 && (
            <p className="text-sm text-stone-600 dark:text-stone-400">
              Not seen in any chat.
            </p>
          )}
        </div>
      </Section>
    </Page>
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

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-2">
      <dt className="text-sm text-stone-600 dark:text-stone-400">{label}</dt>
      <dd className="flex items-center gap-2">{children}</dd>
    </div>
  );
}

function SettingRow({ label, setting }: { label: string; setting: Setting<string> }) {
  return (
    <Row label={label}>
      {setting.value}
      <span
        className={`rounded px-1.5 py-0.5 text-xs font-medium ${
          setting.own
            ? "bg-teal-50 text-teal-800 dark:bg-teal-950 dark:text-teal-300"
            : "bg-stone-100 text-stone-600 dark:bg-stone-800 dark:text-stone-400"
        }`}
      >
        {setting.own ? "Own" : "Default"}
      </span>
    </Row>
  );
}

function Counts({ chat }: { chat: Chat }) {
  return (
    <p className="text-sm text-stone-600 dark:text-stone-400">
      Todos: {chat.open_todos} open, {chat.done_todos} done · Shopping:{" "}
      {chat.to_buy} to buy, {chat.bought} bought
      {chat.todo_reminders && " · Todo reminders on"}
    </p>
  );
}

const card =
  "rounded-lg border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900";

function DmCard({ chat }: { chat: Chat }) {
  return (
    <div className={card}>
      <h3 className="mb-1 flex items-center gap-2 font-medium">
        <LockIcon className="size-5 text-stone-500" />
        Private chat
      </h3>
      <Counts chat={chat} />
      <p className="mt-1 text-xs text-stone-500">
        Private, so only the counts are shown.
      </p>
    </div>
  );
}

function GroupCard({ chat }: { chat: Chat }) {
  return (
    <div className={card}>
      <h3 className="mb-1 flex items-center gap-2 font-medium">
        <GroupsIcon className="size-5 text-teal-700 dark:text-teal-400" />
        {chat.name ?? "Unnamed group"}
      </h3>
      <Counts chat={chat} />
      <GroupLists groupId={chat.id} />
    </div>
  );
}
