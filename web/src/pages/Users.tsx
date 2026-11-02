import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";

import { getJson, type UserList, type UserRow } from "../api";
import LoadError from "../components/LoadError";
import Page from "../components/Page";
import { count, date, timeAgo } from "../format";

export default function Users() {
  const { data, error, isPending } = useQuery({
    queryKey: ["users"],
    queryFn: () => getJson<UserList>("/users"),
  });

  return (
    <Page title="Users">
      {isPending && <p className="text-sm text-stone-500">Loading…</p>}
      {error && <LoadError error={error} />}
      {data && data.users.length === 0 && (
        <p className="text-stone-600 dark:text-stone-400">
          Nobody has messaged the bot yet.
        </p>
      )}
      {data && data.users.length > 0 && (
        <>
          <p className="mb-3 text-sm text-stone-600 dark:text-stone-400">
            {count(data.users.length, "person", "people")}, most recently active
            first.
          </p>
          <ul className="divide-y divide-stone-200 overflow-hidden rounded-lg border border-stone-200 bg-white dark:divide-stone-800 dark:border-stone-800 dark:bg-stone-900">
            {data.users.map((user) => (
              <UserItem key={user.id} user={user} days={data.recent_days} />
            ))}
          </ul>
        </>
      )}
    </Page>
  );
}

/** Within a week, "Cleared on …" is worth showing in the list. */
const SOON_MS = 7 * 86_400_000;

function UserItem({ user, days }: { user: UserRow; days: number }) {
  const details = [
    `Last active ${timeAgo(user.last_active)}`,
    `${count(user.recent_uses, "use")} in ${days} days`,
    count(user.groups, "group"),
    user.has_dm ? "Private chat" : null,
  ].filter(Boolean);

  return (
    <li>
      <Link
        to={`/users/${user.id}`}
        className="flex items-center gap-3 px-4 py-3 hover:bg-stone-50 focus-visible:bg-stone-50 dark:hover:bg-stone-800/60 dark:focus-visible:bg-stone-800/60"
      >
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-2 font-medium">
            {user.name}
            <ClearingBadge user={user} />
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

/** "Cleared", or "Clears 16 Nov" when that's within a week. */
function ClearingBadge({ user }: { user: UserRow }) {
  if (user.cleared_at) {
    return (
      <span className="rounded bg-stone-100 px-1.5 py-0.5 text-xs font-medium text-stone-600 dark:bg-stone-800 dark:text-stone-400">
        Cleared {date(user.cleared_at)}
      </span>
    );
  }
  if (user.clears_on && new Date(user.clears_on).getTime() - Date.now() < SOON_MS) {
    return (
      <span className="rounded bg-amber-50 px-1.5 py-0.5 text-xs font-medium text-amber-800 dark:bg-amber-950 dark:text-amber-300">
        Clears {date(user.clears_on)}
      </span>
    );
  }
  return null;
}
