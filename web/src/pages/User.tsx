import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import GroupsIcon from "@mui/icons-material/Groups";
import LockIcon from "@mui/icons-material/Lock";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Link, useParams } from "react-router";

import { ApiError, getJson, type Chat, type Cleanup, type UserDetail } from "../api";
import { GroupStatus, PersonData } from "../components/Clearing";
import GroupLists from "../components/GroupLists";
import LoadError from "../components/LoadError";
import { ScopedMetrics } from "../components/MetricsReport";
import Page from "../components/Page";
import SettingsEditor from "../components/SettingsEditor";
import Switch from "../components/Switch";
import { count, date, timeAgo } from "../format";
import { useUserChange, userOwner } from "../useChange";

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
        <SettingsEditor
          owner={userOwner(user.id)}
          name={user.name}
          settings={settings}
          where="private chat"
          note={
            dm
              ? "Changes apply straight away. The bot doesn't tell them."
              : `${user.name} hasn't messaged the bot privately, so these are the defaults. A change starts their private chat's settings; the bot doesn't message them about it, but prayer reminders, if turned on, are sent there.`
          }
        />
      </Section>

      <Section title="Data">
        <PersonData user={user} cleanup={data.cleanup} />
      </Section>

      <Section title="Usage">
        <ScopedMetrics scope="user" id={user.id} />
      </Section>

      <Section title="Chats">
        <div className="flex flex-col gap-3">
          {dm && <DmCard chat={dm} userId={user.id} />}
          {groups.map((group) => (
            <GroupCard key={group.id} chat={group} userId={user.id} cleanup={data.cleanup} />
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

function Counts({ chat }: { chat: Chat }) {
  return (
    <p className="text-sm text-stone-600 dark:text-stone-400">
      Todos: {chat.open_todos} open, {chat.done_todos} done · Shopping:{" "}
      {chat.to_buy} to buy, {chat.bought} bought
    </p>
  );
}

/** Their todo reminders in this chat (`/todo reminders on`). */
function TodoReminders({ chat, userId }: { chat: Chat; userId: number }) {
  const change = useUserChange(userId);
  return (
    <div className="mt-2">
      <div className="flex items-center gap-2 text-sm">
        <Switch
          on={chat.todo_reminders}
          label="Todo reminders"
          disabled={change.isPending}
          onChange={(on) =>
            change.mutate({ method: "POST", path: `/chats/${chat.id}/todo-reminders`, body: { on } })
          }
        />
        Todo reminders {chat.todo_reminders ? "on" : "off"}
      </div>
      {change.error && (
        <p role="alert" className="mt-1 text-sm text-red-700 dark:text-red-400">
          {change.error instanceof ApiError && change.error.reason
            ? change.error.reason
            : "Couldn't save that."}
        </p>
      )}
    </div>
  );
}

const card =
  "rounded-lg border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900";

function DmCard({ chat, userId }: { chat: Chat; userId: number }) {
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
      <TodoReminders chat={chat} userId={userId} />
    </div>
  );
}

function GroupCard({ chat, userId, cleanup }: { chat: Chat; userId: number; cleanup: Cleanup }) {
  return (
    <div className={card}>
      <h3 className="mb-1 flex items-center gap-2 font-medium">
        <GroupsIcon className="size-5 text-teal-700 dark:text-teal-400" />
        <Link to={`/groups/${chat.id}`} className="hover:underline">
          {chat.name ?? "Unnamed group"}
        </Link>
      </h3>
      <Counts chat={chat} />
      <TodoReminders chat={chat} userId={userId} />
      <GroupLists groupId={chat.id} />
      <GroupStatus chat={chat} cleanup={cleanup} />
    </div>
  );
}
