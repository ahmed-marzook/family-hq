import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import PersonIcon from "@mui/icons-material/Person";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Link, useNavigate, useParams } from "react-router";

import { getJson, type GroupDetail } from "../api";
import { GroupStatus } from "../components/Clearing";
import GroupLists from "../components/GroupLists";
import LoadError from "../components/LoadError";
import { ScopedMetrics } from "../components/MetricsReport";
import Page from "../components/Page";
import SettingsEditor from "../components/SettingsEditor";
import { count, date, timeAgo } from "../format";
import { groupOwner } from "../useChange";

export default function Group() {
  const { id } = useParams();
  const { data, error, isPending } = useQuery({
    queryKey: ["group", id],
    queryFn: () => getJson<GroupDetail>(`/groups/${id}`),
  });

  return (
    <>
      <Link
        to="/groups"
        className="mb-4 inline-flex items-center gap-1 text-sm text-teal-700 hover:underline dark:text-teal-400"
      >
        <ArrowBackIcon className="size-4" />
        Groups
      </Link>
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} notFound="There's no such group." />}
      {data && <Detail data={data} />}
    </>
  );
}

function Detail({ data }: { data: GroupDetail }) {
  const { group } = data;
  const navigate = useNavigate();
  const name = group.name ?? "Unnamed group";

  return (
    <Page title={name}>
      <p className="mb-6 text-sm text-stone-600 dark:text-stone-400">
        Bot last used here {timeAgo(group.last_active)} ({date(group.last_active)}) ·{" "}
        {count(group.recent_uses, "use")} in the last {data.recent_days} days
      </p>

      <Section title="Settings">
        <SettingsEditor
          owner={groupOwner(group.id)}
          name={name}
          settings={data.settings}
          where="in the group"
          note="The group's own settings, as /settings in the group changes them. Members who set their own in a private chat get prayer reminders on their own times. Changes apply straight away; the bot doesn't tell the group."
        />
      </Section>

      <Section title="Lists">
        <p className="text-sm text-stone-600 dark:text-stone-400">
          Todos: {group.open_todos} open, {group.done_todos} done · Shopping:{" "}
          {group.to_buy} to buy, {group.bought} bought
        </p>
        <GroupLists groupId={group.id} />
      </Section>

      <Section title="Members">
        {data.members.length === 0 ? (
          <p className="text-sm text-stone-600 dark:text-stone-400">
            Nobody in it has used the bot yet.
          </p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {data.members.map((member) => (
              <li key={member.id}>
                <Link
                  to={`/users/${member.id}`}
                  className="inline-flex items-center gap-1.5 rounded-md border border-stone-200 bg-white px-3 py-1.5 text-sm hover:bg-stone-50 dark:border-stone-800 dark:bg-stone-900 dark:hover:bg-stone-800"
                >
                  <PersonIcon className="size-4 text-stone-500" />
                  {member.name}
                </Link>
              </li>
            ))}
          </ul>
        )}
        <p className="mt-2 text-xs text-stone-500">
          People the bot has seen send a message here.
        </p>
      </Section>

      <Section title="Usage">
        <ScopedMetrics scope="chat" id={group.id} />
      </Section>

      <Section title="Bot in this group">
        <GroupStatus
          chat={group}
          cleanup={data.cleanup}
          onLeft={() => navigate("/groups")}
        />
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
