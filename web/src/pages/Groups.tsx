import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";

import { getJson, type GroupList, type GroupRow } from "../api";
import LoadError from "../components/LoadError";
import Page from "../components/Page";
import { count, date, timeAgo } from "../format";

export default function Groups() {
  const { data, error, isPending } = useQuery({
    queryKey: ["groups"],
    queryFn: () => getJson<GroupList>("/groups"),
  });

  return (
    <Page title="Groups">
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} />}
      {data && data.groups.length === 0 && (
        <p className="text-stone-600 dark:text-stone-400">
          The bot hasn't been used in a group yet.
        </p>
      )}
      {data && data.groups.length > 0 && (
        <>
          <p className="mb-3 text-sm text-stone-600 dark:text-stone-400">
            {count(data.groups.length, "group")}, most recently active first.
          </p>
          <ul className="divide-y divide-stone-200 overflow-hidden rounded-lg border border-stone-200 bg-white dark:divide-stone-800 dark:border-stone-800 dark:bg-stone-900">
            {data.groups.map((group) => (
              <GroupItem key={group.id} group={group} days={data.recent_days} />
            ))}
          </ul>
        </>
      )}
    </Page>
  );
}

/** Within a week, "Leaves on …" is worth showing in the list. */
const SOON_MS = 7 * 86_400_000;

function GroupItem({ group, days }: { group: GroupRow; days: number }) {
  const details = [
    `Last used ${timeAgo(group.last_active)}`,
    `${count(group.recent_uses, "use")} in ${days} days`,
    count(group.members, "member"),
    `${group.open_todos} open ${group.open_todos === 1 ? "todo" : "todos"}`,
    `${group.to_buy} to buy`,
  ];

  return (
    <li>
      <Link
        to={`/groups/${group.id}`}
        className="flex items-center gap-3 px-4 py-3 hover:bg-stone-50 focus-visible:bg-stone-50 dark:hover:bg-stone-800/60 dark:focus-visible:bg-stone-800/60"
      >
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-2 font-medium">
            {group.name ?? "Unnamed group"}
            <LeavingBadge group={group} />
          </span>
          <span className="block text-sm text-stone-600 dark:text-stone-400">
            {details.join(" · ")}
          </span>
        </span>
        <ChevronRightIcon className="size-5 shrink-0 text-stone-400" />
      </Link>
    </li>
  );
}

/** "Bot removed", or "Leaves 16 Nov" when that's within a week. */
function LeavingBadge({ group }: { group: GroupRow }) {
  if (group.left_at) {
    return (
      <span className="rounded bg-stone-100 px-1.5 py-0.5 text-xs font-medium text-stone-600 dark:bg-stone-800 dark:text-stone-400">
        Bot removed
      </span>
    );
  }
  if (group.leaves_on && new Date(group.leaves_on).getTime() - Date.now() < SOON_MS) {
    return (
      <span className="rounded bg-amber-50 px-1.5 py-0.5 text-xs font-medium text-amber-800 dark:bg-amber-950 dark:text-amber-300">
        Leaves {date(group.leaves_on)}
      </span>
    );
  }
  return null;
}
