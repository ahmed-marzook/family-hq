-- Phase 1 schema: users, chats, shopping, todos, and the WhatsApp
-- bookkeeping tables.
--
-- Conventions:
-- * STRICT tables: SQLite normally stores any value in any column. STRICT
--   makes it reject e.g. text in an INTEGER column, like a normal database.
-- * Times are TEXT in UTC, RFC 3339 (e.g. '2026-10-27T18:12:30Z').
-- * CHECK constraints are a safety net. Rust validates first, so users get
--   friendly messages; these catch bugs that slip past.
-- * Foreign keys are enforced (SQLx turns `PRAGMA foreign_keys` on).
--
-- Never edit this file once it has run anywhere. Schema changes go in a
-- new file, 0002_....sql.

-- People who can use the bot. Messages from anyone else are ignored.
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
