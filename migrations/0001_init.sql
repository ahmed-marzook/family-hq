-- Initial schema: users, chats and their settings, shopping, todos,
-- prayer reminders and tracking, and the WhatsApp bookkeeping tables.
--
-- Conventions:
-- * STRICT tables: SQLite normally stores any value in any column. STRICT
--   makes it reject e.g. text in an INTEGER column, like a normal database.
-- * Times are TEXT in UTC, RFC 3339 (e.g. '2026-10-27T18:12:30Z'). Dates
--   (YYYY-MM-DD) are in the chat's own timezone.
-- * CHECK constraints are a safety net. Rust validates first, so users get
--   friendly messages; these catch bugs that slip past.
-- * Foreign keys are enforced (SQLx turns `PRAGMA foreign_keys` on).
--
-- Never edit this file once it has run anywhere. Schema changes go in a
-- new file, 0002_....sql.

-- People who use the bot. Registered automatically on their first message.
CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The name used in `+todo @Khadija`. NOCASE makes comparisons (and the
    -- UNIQUE check) ignore case, so 'khadija' finds 'Khadija' and a second
    -- 'KHADIJA' is rejected.
    display_name  TEXT NOT NULL UNIQUE COLLATE NOCASE
                  CHECK (length(trim(display_name)) > 0),
    -- Phone-number ID: '447700900123@s.whatsapp.net'.
    wa_jid        TEXT NOT NULL UNIQUE CHECK (wa_jid LIKE '%@s.whatsapp.net'),
    -- The same person's LID: '100000000000001@lid'. Group messages and
    -- member lists may use this instead of the phone-number ID.
    wa_lid        TEXT UNIQUE CHECK (wa_lid IS NULL OR wa_lid LIKE '%@lid'),
    created_at    TEXT NOT NULL
) STRICT;

-- Every WhatsApp chat the bot works in: a group, or a DM with one user.
-- Every list item belongs to exactly one chat, so each group has its own
-- lists, and a DM is that person's private list.
CREATE TABLE chats (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- '120363...@g.us' for a group, '4477...@s.whatsapp.net' for a DM.
    wa_chat_id  TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL CHECK (kind IN ('group', 'dm')),
    -- The group's name, for showing in lists and logs. NULL for DMs.
    name        TEXT,
    -- Whose DM this is. UNIQUE: one DM chat per user. (UNIQUE allows any
    -- number of NULLs, so groups aren't affected.)
    dm_user_id  INTEGER UNIQUE REFERENCES users(id),
    created_at  TEXT NOT NULL,
    -- A DM has an owner and a group doesn't. `(a) = (b)` on two
    -- true/false tests means "both true or both false".
    CHECK ((kind = 'dm') = (dm_user_id IS NOT NULL)),
    -- Group IDs end in @g.us, and nothing else does.
    CHECK ((kind = 'group') = (wa_chat_id LIKE '%@g.us'))
) STRICT;

-- Who is in which group. Used to decide who `@Khadija` can refer to in a
-- given group, so separate families' groups don't mix.
CREATE TABLE chat_members (
    chat_id    INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    joined_at  TEXT NOT NULL,
    -- A composite primary key: each (chat, user) pair at most once.
    PRIMARY KEY (chat_id, user_id)
) STRICT;

CREATE TABLE shopping_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Which chat's list this is on.
    chat_id     INTEGER NOT NULL REFERENCES chats(id),
    name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
    added_by    INTEGER NOT NULL REFERENCES users(id),
    bought_by   INTEGER REFERENCES users(id),      -- NULL = still to buy
    bought_at   TEXT,
    created_at  TEXT NOT NULL,
    -- Bought means both who and when; not bought means neither.
    CHECK ((bought_by IS NULL) = (bought_at IS NULL))
) STRICT;

-- `/shop` reads "items still to buy in this chat", so index exactly that.
CREATE INDEX shopping_items_by_chat ON shopping_items (chat_id, bought_at);

CREATE TABLE todos (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Which chat's list this is on. A DM chat = a personal todo.
    chat_id       INTEGER NOT NULL REFERENCES chats(id),
    title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
    created_by    INTEGER NOT NULL REFERENCES users(id),
    -- Who should do it. The sender when no `@name` is given.
    assignee_id   INTEGER REFERENCES users(id),
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'completed')),
    completed_by  INTEGER REFERENCES users(id),
    completed_at  TEXT,
    created_at    TEXT NOT NULL,
    -- Completed means both who and when; pending means neither.
    CHECK ((status = 'completed') = (completed_at IS NOT NULL)),
    CHECK ((completed_by IS NULL) = (completed_at IS NULL))
) STRICT;

CREATE INDEX todos_by_chat ON todos (chat_id, status);

-- Every WhatsApp message already handled. GOWA retries webhooks, so the
-- same message can arrive twice; inserting its ID first and skipping on a
-- duplicate-key error makes handling it twice impossible.
CREATE TABLE processed_messages (
    wa_message_id  TEXT PRIMARY KEY,
    received_at    TEXT NOT NULL
) STRICT;

-- "Item 2 in the list the bot sent in message X" → the real row.
-- Numbers are frozen per list message, so replying to an old list still
-- refers to the items it showed.
CREATE TABLE wa_message_context (
    wa_message_id  TEXT NOT NULL,       -- the bot's list/confirmation message
    -- The chat the list was sent in. A reply is only valid in that chat.
    chat_id        INTEGER NOT NULL REFERENCES chats(id),
    position       INTEGER NOT NULL CHECK (position >= 1),  -- the number shown
    item_type      TEXT NOT NULL CHECK (item_type IN ('todo', 'shop')),
    -- No foreign key: it points at one of two tables depending on
    -- item_type. The code checks the row still exists when it's used.
    item_id        INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (wa_message_id, position)
) STRICT;

-- Settings for one chat. A DM's row is that person's own settings (their
-- personal reminders arrive in that DM); a group's row is shared by
-- everyone in it. A chat with no row, or NULL in a column, uses the
-- server default from .env, so changing a default reaches every chat
-- that hasn't chosen its own.
CREATE TABLE chat_settings (
    chat_id                INTEGER PRIMARY KEY REFERENCES chats(id),
    -- IANA name, e.g. 'Europe/London'. Decides what "today" means and how
    -- times are shown. Checked in Rust (the list of names is too long for
    -- a CHECK); an unknown name falls back to the default.
    timezone               TEXT CHECK (timezone IS NULL OR length(trim(timezone)) > 0),
    city                   TEXT CHECK (city IS NULL OR length(trim(city)) > 0),
    country                TEXT CHECK (country IS NULL OR length(trim(country)) > 0),
    -- 1 = send prayer reminders here (`/prayers on`), 0 = don't.
    -- SQLite has no boolean type; 0/1 integers are the convention.
    prayer_reminders       INTEGER NOT NULL DEFAULT 0 CHECK (prayer_reminders IN (0, 1)),
    -- AlAdhan calculation method: 0-23 (99, "custom", needs more settings
    -- than we store, so it isn't allowed).
    prayer_method          INTEGER CHECK (prayer_method IS NULL OR prayer_method BETWEEN 0 AND 23),
    -- Asr: 0 = Shafi'i (AlAdhan's "standard"), 1 = Hanafi.
    prayer_school          INTEGER CHECK (prayer_school IS NULL OR prayer_school IN (0, 1)),
    -- How many minutes before a prayer's time ends the "ending soon"
    -- reminder goes out.
    prayer_ending_minutes  INTEGER
                           CHECK (prayer_ending_minutes IS NULL OR prayer_ending_minutes BETWEEN 1 AND 120),
    -- A city only makes sense with its country: both set, or neither.
    CHECK ((city IS NULL) = (country IS NULL))
) STRICT;

-- One reminder the bot sent (or is sending) for one prayer, in one chat.
-- Each prayer gets two: when it starts, and shortly before it ends.
-- People react to either to say they've prayed, so its WhatsApp message
-- ID is how a reaction is matched back to a prayer and a day.
CREATE TABLE prayer_reminders (
    id             INTEGER PRIMARY KEY,
    chat_id        INTEGER NOT NULL REFERENCES chats(id),
    -- The day in Europe/London ('2026-10-27'). date() gives back exactly
    -- the same text only for a real date in this format, so this rejects
    -- '2026-02-31' and '27-10-2026' alike.
    prayer_date    TEXT NOT NULL CHECK (date(prayer_date) IS prayer_date),
    prayer         TEXT NOT NULL
                   CHECK (prayer IN ('fajr', 'dhuhr', 'asr', 'maghrib', 'isha')),
    -- 'start': the prayer's time has begun. 'ending': it ends soon.
    kind           TEXT NOT NULL CHECK (kind IN ('start', 'ending')),
    prayer_time    TEXT NOT NULL,       -- when the prayer starts, UTC
    -- Set once WhatsApp has accepted the message. NULL if sending failed.
    wa_message_id  TEXT UNIQUE,
    sent_at        TEXT NOT NULL,
    -- At most one of each kind per prayer per day per chat. The reminder
    -- loop inserts first and only sends if the insert worked, so a restart
    -- or a second check can never send the same reminder twice.
    UNIQUE (chat_id, prayer_date, prayer, kind)
) STRICT;

-- Who has prayed what. Per person, not per chat: reacting in any chat
-- counts, and reacting in two chats is still one prayer.
CREATE TABLE prayer_log (
    user_id      INTEGER NOT NULL REFERENCES users(id),
    prayer_date  TEXT NOT NULL CHECK (date(prayer_date) IS prayer_date),
    prayer       TEXT NOT NULL
                 CHECK (prayer IN ('fajr', 'dhuhr', 'asr', 'maghrib', 'isha')),
    prayed_at    TEXT NOT NULL,
    PRIMARY KEY (user_id, prayer_date, prayer)
) STRICT;
