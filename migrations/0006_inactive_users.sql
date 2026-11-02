-- Clearing the data of people, and groups, that stop using the bot, and of
-- groups the bot is no longer in (src/cleanup.rs).
--
-- "Using the bot" means a command (or a `+word` it didn't know), a reply to
-- one of its lists, a reaction to a reminder, or any message in their DM.
-- Ordinary chat in a group doesn't count.

-- When they last used the bot. Every message that counts sets it.
ALTER TABLE users ADD COLUMN last_active_at TEXT;

-- When the bot warned them (in their DM) that clearing is coming. NULL
-- until then, and NULL again as soon as they're active.
ALTER TABLE users ADD COLUMN warned_at TEXT;

-- When their data was cleared. The row itself is only kept while a group's
-- todo or shopping list still names them; NULL while they're active.
ALTER TABLE users ADD COLUMN cleared_at TEXT;

-- When the bot was first seen to be no longer in this group (removed, or
-- the group deleted). A day later the group and its lists are deleted;
-- NULL while the bot is in it.
ALTER TABLE chats ADD COLUMN left_at TEXT;

-- A group's own activity: when someone last used the bot in it, and when
-- the bot warned the group it would leave. (A DM's activity is its
-- owner's, in `users`.)
ALTER TABLE chats ADD COLUMN last_active_at TEXT;
ALTER TABLE chats ADD COLUMN warned_at TEXT;

-- Everyone, and every chat, starts from their latest logged use, or when
-- they joined.
UPDATE users SET last_active_at = coalesce(
    (SELECT max(l.at) FROM command_log l WHERE l.user_id = users.id),
    created_at
);
UPDATE chats SET last_active_at = coalesce(
    (SELECT max(l.at) FROM command_log l WHERE l.chat_id = chats.id),
    created_at
);
