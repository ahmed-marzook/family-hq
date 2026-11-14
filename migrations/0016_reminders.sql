-- One table for every reminder the bot sends, whichever feature it's for
-- (Stage 2.2's shared engine). Each feature keeps its own record of *what*
-- was due (`prayer_reminders`, `todo_reminders`); this row is the reminder
-- message itself: which feature owns it (`source`, `source_id`), when it's
-- due, and its WhatsApp ID once sent, so a reaction to it can be routed
-- back to that feature.
CREATE TABLE reminders (
    id             INTEGER PRIMARY KEY,
    chat_id        INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    -- When it should go out (UTC).
    due_at         TEXT NOT NULL,
    source         TEXT NOT NULL CHECK (source IN ('prayer', 'todo', 'custom')),
    -- The owning feature's row: a `prayer_reminders` id for 'prayer', a
    -- todo's id for 'todo' (NULL for the four-a-day list of todos, which
    -- is answered by number instead).
    source_id      INTEGER,
    -- What to send, for reminders written ahead of time. '' for the ones a
    -- feature works out and sends on the spot: their text is that
    -- feature's to rebuild, and a copy here (with phone numbers and list
    -- contents) would be one more thing to clear when someone leaves.
    text           TEXT NOT NULL DEFAULT '',
    -- Phone numbers to @mention, separated by spaces.
    mentions       TEXT NOT NULL DEFAULT '',
    -- Set by the sending loop just before it sends. Only a row still NULL
    -- here is ever sent, and the loop sets it first, so nothing goes
    -- twice; a row due while the bot was down is still NULL, so it goes
    -- out on start-up.
    claimed_at     TEXT,
    -- Set once WhatsApp has accepted the message. NULL if sending failed.
    wa_message_id  TEXT UNIQUE
) STRICT;

-- The sending loop's question every minute: what's due and not claimed.
CREATE INDEX reminders_unclaimed ON reminders (due_at) WHERE claimed_at IS NULL;

-- Prayer reminders already sent, so reactions on them keep working after
-- this change. From now on their message IDs are only kept here.
INSERT INTO reminders (chat_id, due_at, source, source_id, claimed_at, wa_message_id)
SELECT chat_id, sent_at, 'prayer', id, sent_at, wa_message_id
FROM prayer_reminders
WHERE wa_message_id IS NOT NULL;
