-- Stage 1.7: todos get a due date and a priority, and can be skipped:
-- closed as "won't be done", and kept as a record of that rather than
-- deleted.
--
-- SQLite can't change a CHECK constraint in place, and `status` needs a
-- third value, so `todos` is rebuilt: a new table, the rows copied across,
-- the old one dropped and the new one renamed. Nothing has a foreign key
-- to `todos`, so dropping it is safe.
--
-- `completed_by` / `completed_at` become `closed_by` / `closed_at`: who
-- closed the todo and when, whether it was done or skipped.
CREATE TABLE todos_new (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Which chat's list this is on. A DM chat = a personal todo.
    chat_id       INTEGER NOT NULL REFERENCES chats(id),
    title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
    created_by    INTEGER NOT NULL REFERENCES users(id),
    -- Who should do it. The sender when no `@name` is given.
    assignee_id   INTEGER REFERENCES users(id),
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'completed', 'skipped')),
    closed_by     INTEGER REFERENCES users(id),
    closed_at     TEXT,
    -- The day it's due, as a plain date ("2026-11-13", no time or zone):
    -- worked out on the sender's own clock when it was added. NULL = no
    -- due date. `date(x, '+0 days') IS x` only holds for a real YYYY-MM-DD
    -- date (and for NULL), so "13/11" can't get in. The '+0 days' matters:
    -- plain `date('2026-02-30')` gives '2026-02-30' back unchanged, but
    -- with a modifier SQLite works it out as '2026-03-02', which differs.
    due_date      TEXT CHECK (date(due_date, '+0 days') IS due_date),
    -- NULL = no priority given.
    priority      TEXT CHECK (priority IN ('high', 'medium', 'low')),
    created_at    TEXT NOT NULL,
    -- Closed (done or skipped) means both who and when; pending, neither.
    CHECK ((status = 'pending') = (closed_at IS NULL)),
    CHECK ((closed_by IS NULL) = (closed_at IS NULL))
) STRICT;

INSERT INTO todos_new (id, chat_id, title, created_by, assignee_id, status,
                       closed_by, closed_at, created_at)
SELECT id, chat_id, title, created_by, assignee_id, status,
       completed_by, completed_at, created_at
FROM todos;

-- AUTOINCREMENT never reuses an id, because SQLite remembers the highest
-- one in `sqlite_sequence`. Carry that over, so a deleted todo's id isn't
-- given to a new one: old `/todo` lists (`wa_message_context`) still point
-- at ids, and "done 5" on an old list must not tick off a new todo.
DELETE FROM sqlite_sequence WHERE name = 'todos_new';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'todos_new', seq FROM sqlite_sequence WHERE name = 'todos';

DROP TABLE todos;
-- Renaming also renames its `sqlite_sequence` row.
ALTER TABLE todos_new RENAME TO todos;

-- Dropped with the old table, so made again.
CREATE INDEX todos_by_chat ON todos (chat_id, status);
