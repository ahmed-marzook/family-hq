-- Jot: an opt-in log of what someone has done, prompted every N hours
-- during their active hours (DM only). The bot's own prompts go out on the
-- shared reminder engine; these tables are Jot's own record of them.

-- One row per person who has ever turned it on. `enabled` is what `/jot
-- off` clears, so their interval and hours are remembered.
CREATE TABLE jot_settings (
    user_id          INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    enabled          INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    interval_minutes INTEGER NOT NULL CHECK (interval_minutes BETWEEN 30 AND 240),
    -- Local clock times in the person's own timezone, "HH:MM". Zero-padded,
    -- so comparing the text compares the times.
    start_local      TEXT NOT NULL CHECK (start_local GLOB '[0-2][0-9]:[0-5][0-9]'),
    end_local        TEXT NOT NULL CHECK (end_local GLOB '[0-2][0-9]:[0-5][0-9]'),
    -- When it was last turned on or changed (UTC). Prompts before this are
    -- never sent, so turning it on at 11:10 doesn't send the 11:00 one.
    updated_at       TEXT NOT NULL,
    CHECK (start_local < end_local)
) STRICT;

-- What people jotted down.
CREATE TABLE jot_entries (
    id        INTEGER PRIMARY KEY,
    user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    logged_at TEXT NOT NULL,
    -- The local date it was logged on, in the person's timezone at the time.
    day       TEXT NOT NULL,
    -- The prompt it answers (UTC). NULL for `+jot`.
    slot_at   TEXT,
    text      TEXT NOT NULL
) STRICT;

CREATE INDEX jot_entries_by_day ON jot_entries (user_id, day);

-- Each prompt, claimed before it's sent (so it goes out once) and given its
-- WhatsApp ID once sent, so a reply quoting it finds its slot.
CREATE TABLE jot_prompts (
    id            INTEGER PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    chat_id       INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    slot_at       TEXT NOT NULL,
    claimed_at    TEXT NOT NULL,
    wa_message_id TEXT UNIQUE,
    UNIQUE (user_id, slot_at)
) STRICT;
