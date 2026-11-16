-- Jot J4: logging a Jot slot earns a point, and a day with every scored
-- slot logged earns a bonus (see `points::for_jot_slot` / `for_jot_day`).
--
-- Both are source 'jot', told apart by their reason:
-- - 'jot_slot': `source_id` is the slot's moment as Unix seconds. A slot
--   pays once, however many entries answer it (an edited reply, a second
--   message, a `+jot` on top).
-- - 'jot_full_day': `source_id` is the day as YYYYMMDD, once per day.
-- Slot moments are per person (two people both have a 09:00 slot), so the
-- unique index is per person: `(user_id, reason, source_id)`.
--
-- That needs new CHECK values, and SQLite can't change a table's CHECKs in
-- place, so (as in 0013) this creates the new shape, copies the rows over,
-- drops the old and renames.
CREATE TABLE points_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    chat_id    INTEGER NOT NULL REFERENCES chats(id),
    source     TEXT NOT NULL
               CHECK (source IN ('todo', 'prayer', 'overdue', 'jot', 'habit', 'goal')),
    -- A todo's id (for 'todo' and 'overdue'), for a prayer which one
    -- (1 Fajr … 5 Isha), for Jot the slot or the day (see above).
    source_id  INTEGER NOT NULL,
    reason     TEXT NOT NULL
               CHECK (reason IN ('done', 'done_on_time', 'done_late', 'skipped',
                                 'prayed', 'made_up', 'overdue',
                                 'jot_slot', 'jot_full_day')),
    points     INTEGER NOT NULL CHECK (points BETWEEN -10 AND 10),
    -- The earner's local day. For a prayer, the day it belongs to; for an
    -- overdue charge, the day it was charged for; for Jot, the slot's day.
    day        TEXT NOT NULL CHECK (date(day, '+0 days') IS day),
    at         TEXT NOT NULL,
    CHECK (source <> 'prayer' OR source_id BETWEEN 1 AND 5),
    -- An overdue charge only ever costs; it never earns.
    CHECK ((source = 'overdue') = (reason = 'overdue')),
    CHECK (source <> 'overdue' OR points < 0),
    -- Jot only ever earns: a missed slot simply isn't a logged one.
    CHECK ((source = 'jot') = (reason IN ('jot_slot', 'jot_full_day'))),
    CHECK (source <> 'jot' OR points > 0)
) STRICT;

INSERT INTO points_new (id, user_id, chat_id, source, source_id, reason, points, day, at)
SELECT id, user_id, chat_id, source, source_id, reason, points, day, at
FROM points;

DROP TABLE points;
ALTER TABLE points_new RENAME TO points;

CREATE INDEX points_by_chat_day ON points (chat_id, day);
CREATE INDEX points_by_user_day ON points (user_id, day);
-- A todo awards once (as before; overdue charges and Jot are kept out).
CREATE UNIQUE INDEX points_once_per_todo ON points (source, source_id)
    WHERE source NOT IN ('prayer', 'overdue', 'jot');
-- Each person's prayer on a day has one row (as before).
CREATE UNIQUE INDEX points_once_per_prayer ON points (user_id, day, source_id)
    WHERE source = 'prayer';
-- An overdue todo is charged at most once a day (as before).
CREATE UNIQUE INDEX points_once_per_overdue_day ON points (source_id, day)
    WHERE source = 'overdue';
-- A person's Jot slot pays once, and so does their full day.
CREATE UNIQUE INDEX points_once_per_jot ON points (user_id, reason, source_id)
    WHERE source = 'jot';
