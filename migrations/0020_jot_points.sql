-- Jot earns points: 1 for each prompt that got an entry, and a bonus for a
-- day where every prompt did (see `points::for_jot_slot`). That needs a new
-- source and two new reasons, and SQLite can't change a table's CHECKs in
-- place, so (as in 0011 and 0013) this creates the new shape, copies the
-- rows over, drops the old and renames.
CREATE TABLE points_new (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    chat_id    INTEGER NOT NULL REFERENCES chats(id),
    source     TEXT NOT NULL
               CHECK (source IN ('todo', 'prayer', 'overdue', 'jot', 'habit', 'goal')),
    -- A todo's id (for 'todo' and 'overdue'); for a prayer, which one
    -- (1 Fajr … 5 Isha); for Jot, the prompt's time as Unix seconds (a
    -- slot) or the local day as days since 0001-01-01 (a full day).
    source_id  INTEGER NOT NULL,
    reason     TEXT NOT NULL
               CHECK (reason IN ('done', 'done_on_time', 'done_late', 'skipped',
                                 'prayed', 'made_up', 'overdue', 'jotted', 'jot_day')),
    points     INTEGER NOT NULL CHECK (points BETWEEN -10 AND 10),
    day        TEXT NOT NULL CHECK (date(day, '+0 days') IS day),
    at         TEXT NOT NULL,
    CHECK (source <> 'prayer' OR source_id BETWEEN 1 AND 5),
    CHECK ((source = 'overdue') = (reason = 'overdue')),
    CHECK (source <> 'overdue' OR points < 0),
    CHECK ((source = 'jot') = (reason IN ('jotted', 'jot_day'))),
    -- Jot only ever earns.
    CHECK (source <> 'jot' OR points > 0)
) STRICT;

INSERT INTO points_new (id, user_id, chat_id, source, source_id, reason, points, day, at)
SELECT id, user_id, chat_id, source, source_id, reason, points, day, at
FROM points;

DROP TABLE points;
ALTER TABLE points_new RENAME TO points;

CREATE INDEX points_by_chat_day ON points (chat_id, day);
CREATE INDEX points_by_user_day ON points (user_id, day);
CREATE UNIQUE INDEX points_once_per_todo ON points (source, source_id)
    WHERE source NOT IN ('prayer', 'overdue', 'jot');
CREATE UNIQUE INDEX points_once_per_prayer ON points (user_id, day, source_id)
    WHERE source = 'prayer';
CREATE UNIQUE INDEX points_once_per_overdue_day ON points (source_id, day)
    WHERE source = 'overdue';
-- A slot, or a day, pays once per person, however many times it's answered.
CREATE UNIQUE INDEX points_once_per_jot ON points (user_id, reason, source_id)
    WHERE source = 'jot';
