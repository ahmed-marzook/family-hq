-- Prayers earn points too: 2 for one prayed on time, 1 for one made up,
-- nothing for one missed (see `points::for_prayer`).
--
-- `points` needs a new source ('prayer') and two new reasons. SQLite can't
-- change a table's CHECKs in place, so (as in 0007) this creates the new
-- shape, copies the rows over, drops the old and renames.
--
-- A todo awards once, ever, so it was `UNIQUE (source, source_id)`. A
-- prayer is different: it's one of five on one day for one person, and its
-- points change when the log does (made up, corrected, unticked). So for a
-- prayer, `source_id` is which of the five it is (1 Fajr … 5 Isha), `day`
-- is the prayer's date, and the row is kept in step with `prayer_log`.
-- Each kind gets its own *partial* unique index (one with a `WHERE`): it
-- only covers the rows that match.
CREATE TABLE points_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    chat_id    INTEGER NOT NULL REFERENCES chats(id),
    source     TEXT NOT NULL CHECK (source IN ('todo', 'prayer', 'habit', 'goal')),
    -- A todo's id, or for a prayer, which one (1 Fajr … 5 Isha).
    source_id  INTEGER NOT NULL,
    reason     TEXT NOT NULL
               CHECK (reason IN ('done', 'done_on_time', 'done_late', 'skipped',
                                 'prayed', 'made_up')),
    points     INTEGER NOT NULL CHECK (points BETWEEN -10 AND 10),
    -- The earner's local day. For a prayer, the day it belongs to (a made
    -- up Isha counts on its own day, not the day it was made up).
    day        TEXT NOT NULL CHECK (date(day, '+0 days') IS day),
    at         TEXT NOT NULL,
    CHECK (source <> 'prayer' OR source_id BETWEEN 1 AND 5)
) STRICT;

INSERT INTO points_new (id, user_id, chat_id, source, source_id, reason, points, day, at)
SELECT id, user_id, chat_id, source, source_id, reason, points, day, at
FROM points;

DROP TABLE points;
ALTER TABLE points_new RENAME TO points;

CREATE INDEX points_by_chat_day ON points (chat_id, day);
CREATE INDEX points_by_user_day ON points (user_id, day);
-- A todo awards once (as before).
CREATE UNIQUE INDEX points_once_per_todo ON points (source, source_id)
    WHERE source <> 'prayer';
-- Each person's prayer on a day has one row.
CREATE UNIQUE INDEX points_once_per_prayer ON points (user_id, day, source_id)
    WHERE source = 'prayer';
