import DeleteForeverIcon from "@mui/icons-material/DeleteForever";
import LogoutIcon from "@mui/icons-material/Logout";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";

import { ApiError, sendJson, type Chat, type Cleanup, type UserRow } from "../api";
import { date, timeAgo } from "../format";

const danger =
  "inline-flex items-center gap-1.5 rounded-md border border-red-300 px-3 py-1.5 text-sm font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-900 dark:text-red-400 dark:hover:bg-red-950";

/** "Couldn't …": the API's reason when it gave one. */
function Problem({ error }: { error: Error | null }) {
  if (!error) return null;
  const text =
    error instanceof ApiError && error.reason
      ? error.reason
      : "That didn't work. Is family-hq running?";
  return (
    <p role="alert" className="mt-2 text-sm text-red-700 dark:text-red-400">
      {text}
    </p>
  );
}

/** A date minus some days, for "warned on …". */
function daysBefore(iso: string, days: number): string {
  return new Date(new Date(iso).getTime() - days * 86_400_000).toISOString();
}

/**
 * A person's data: when they last used the bot, when it would be cleared,
 * and a button to clear it now.
 */
export function PersonData({ user, cleanup }: { user: UserRow; cleanup: Cleanup }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const clear = useMutation({
    mutationFn: () => sendJson<{ deleted: boolean }>("POST", `/users/${user.id}/clear`, {}),
    onSuccess: ({ deleted }) => {
      if (deleted) {
        // Drop the cached list (it isn't on screen, so it wouldn't be
        // refetched until shown), so the users page loads it fresh rather
        // than showing them for a moment.
        queryClient.removeQueries({ queryKey: ["users"] });
        queryClient.removeQueries({ queryKey: ["user", String(user.id)] });
        navigate("/users");
      } else {
        queryClient.invalidateQueries({ queryKey: ["users"] });
        queryClient.invalidateQueries({ queryKey: ["user", String(user.id)] });
      }
    },
  });

  let status: string;
  if (user.cleared_at) {
    status = `Their data was cleared on ${date(user.cleared_at)}. Only their name and number are kept, because a group's list still mentions them; they go once it doesn't.`;
  } else if (cleanup.inactive_days <= 0) {
    status =
      "Clearing the data of people who stop using the bot is off (INACTIVE_DAYS on the server).";
  } else if (user.clears_on && user.warned_at) {
    status = `Warned in their private chat on ${date(user.warned_at)}. Their data will be cleared on ${date(user.clears_on)} unless they use the bot.`;
  } else if (user.clears_on && user.has_dm) {
    status = `If they don't use the bot, they'll be warned around ${date(daysBefore(user.clears_on, cleanup.warn_days))} and their data cleared on ${date(user.clears_on)}.`;
  } else if (user.clears_on) {
    status = `If they don't use the bot, their data will be cleared on ${date(user.clears_on)} (no private chat, so no warning).`;
  } else {
    status = "";
  }

  return (
    <>
      <p className="text-sm">
        Last used the bot {timeAgo(user.last_active)} ({date(user.last_active)}).
      </p>
      {status && <p className="mt-1 text-sm text-stone-600 dark:text-stone-400">{status}</p>}
      {!user.cleared_at && (
        <button
          type="button"
          className={`${danger} mt-3`}
          disabled={clear.isPending}
          onClick={() => {
            if (
              window.confirm(
                `Delete ${user.name}'s private chat, lists, settings, reminders, group memberships and usage history now? Group lists stay. This can't be undone.`,
              )
            ) {
              clear.mutate();
            }
          }}
        >
          <DeleteForeverIcon className="size-4" />
          Clear their data now
        </button>
      )}
      <Problem error={clear.error} />
    </>
  );
}

/**
 * A group's state: whether the bot is still in it, when it would leave a
 * quiet one, and a button to leave now.
 */
export function GroupStatus({
  chat,
  userId,
  cleanup,
}: {
  chat: Chat;
  userId: number;
  cleanup: Cleanup;
}) {
  const queryClient = useQueryClient();
  const leave = useMutation({
    mutationFn: () => sendJson<{ left: boolean }>("POST", `/chats/${chat.id}/leave`, {}),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["users"] });
      queryClient.invalidateQueries({ queryKey: ["user", String(userId)] });
    },
  });
  const name = chat.name ?? "this group";

  let status: string;
  if (chat.left_at) {
    status = `The bot isn't in this group any more (noticed ${timeAgo(chat.left_at)}). It's deleted a day after that.`;
  } else if (cleanup.inactive_days <= 0) {
    status = `Bot last used here ${timeAgo(chat.last_active)}.`;
  } else if (chat.leaves_on && chat.warned_at) {
    status = `Warned on ${date(chat.warned_at)} that nobody uses the bot here. It leaves on ${date(chat.leaves_on)} unless someone does.`;
  } else if (chat.leaves_on) {
    status = `Bot last used here ${timeAgo(chat.last_active)}. If nobody uses it, it warns the group, then leaves on ${date(chat.leaves_on)}.`;
  } else {
    status = "";
  }

  return (
    <div className="mt-2">
      {status && <p className="text-xs text-stone-500">{status}</p>}
      {!chat.left_at && (
        <button
          type="button"
          className={`${danger} mt-2`}
          disabled={leave.isPending}
          onClick={() => {
            if (
              window.confirm(
                `Make the bot leave ${name}, and delete its lists and settings for everyone in it? To bring the bot back, someone in the group has to add its number again.`,
              )
            ) {
              leave.mutate();
            }
          }}
        >
          <LogoutIcon className="size-4" />
          Leave and delete now
        </button>
      )}
      <Problem error={leave.error} />
    </div>
  );
}
