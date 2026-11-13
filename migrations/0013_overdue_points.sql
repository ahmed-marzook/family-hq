-- An overdue todo costs its owner a point a day (the owner's request,
-- 2026-11-13): −1 on each of the 7 days after its due date while it's
-- still open (see `points::for_overdue`). It goes to the person the todo
-- is for: its assignee, or whoever added it.
--
-- A todo's own award (done or skipped) is once per todo, but an overdue
-- charge is once per todo *per day*, so it gets a source of its own,
-- 'overdue', with `source_id` the todo's id and `day` the owner's local
-- day it was charged on. That needs new CHECK values, and SQLite can't
-- change a table's CHECKs in place, so (as in 0011) this creates the new
-- shape, copies the rows over, drops the old and renames.
CREATE TABLE points_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    chat_id    INTEGER NOT NULL REFERENCES chats(id),
    source     TEXT NOT NULL
               CHECK (source IN ('todo', 'prayer', 'overdue', 'habit', 'goal')),
    -- A todo's id (for 'todo' and 'overdue'), or for a prayer, which one
    -- (1 Fajr … 5 Isha).
    source_id  INTEGER NOT NULL,
    reason     TEXT NOT NULL
               CHECK (reason IN ('done', 'done_on_time', 'done_late', 'skipped',
                                 'prayed', 'made_up', 'overdue')),
    points     INTEGER NOT NULL CHECK (points BETWEEN -10 AND 10),
    -- The earner's local day. For a prayer, the day it belongs to; for an
    -- overdue charge, the day it was charged for.
    day        TEXT NOT NULL CHECK (date(day, '+0 days') IS day),
    at         TEXT NOT NULL,
    CHECK (source <> 'prayer' OR source_id BETWEEN 1 AND 5),
    -- An overdue charge only ever costs; it never earns.
    CHECK ((source = 'overdue') = (reason = 'overdue')),
    CHECK (source <> 'overdue' OR points < 0)
) STRICT;

INSERT INTO points_new (id, user_id, chat_id, source, source_id, reason, points, day, at)
SELECT id, user_id, chat_id, source, source_id, reason, points, day, at
FROM points;

DROP TABLE points;
ALTER TABLE points_new RENAME TO points;

CREATE INDEX points_by_chat_day ON points (chat_id, day);
CREATE INDEX points_by_user_day ON points (user_id, day);
-- A todo awards once (as before; overdue charges are kept out of it).
CREATE UNIQUE INDEX points_once_per_todo ON points (source, source_id)
    WHERE source NOT IN ('prayer', 'overdue');
-- Each person's prayer on a day has one row (as before).
CREATE UNIQUE INDEX points_once_per_prayer ON points (user_id, day, source_id)
    WHERE source = 'prayer';
-- An overdue todo is charged at most once a day.
CREATE UNIQUE INDEX points_once_per_overdue_day ON points (source_id, day)
    WHERE source = 'overdue';
