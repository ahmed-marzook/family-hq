-- Points: closing a todo earns (or, for a skip, costs) points, so each
-- person can see how their day and week went. Habits will add theirs here
-- later (`source = 'habit'`).
--
-- A ledger: one row per award, written when the todo is closed. Points
-- aren't worked out from `todos` on the fly, because `/todo clear done`
-- deletes closed todos, and the points must outlive them.
CREATE TABLE points (
    id         INTEGER PRIMARY KEY,
    -- Who earned it: whoever ticked the todo off or skipped it.
    user_id    INTEGER NOT NULL REFERENCES users(id),
    -- Which chat it was earned in. A group's leaderboard counts only its own.
    chat_id    INTEGER NOT NULL REFERENCES chats(id),
    -- What earned it. 'habit' and 'goal' are for Stage 2.5, allowed already
    -- so they need no table rebuild.
    source     TEXT NOT NULL CHECK (source IN ('todo', 'habit', 'goal')),
    -- The todo's id. No foreign key: the todo may be cleared, and its
    -- points stay.
    source_id  INTEGER NOT NULL,
    -- Why: done (no due date), done on or before its due date, done after
    -- it, or skipped.
    reason     TEXT NOT NULL
               CHECK (reason IN ('done', 'done_on_time', 'done_late', 'skipped')),
    -- Can be negative (a skip). Bounded, as a safety net against a bug.
    points     INTEGER NOT NULL CHECK (points BETWEEN -10 AND 10),
    -- The earner's local day ("2026-11-06"), worked out on their own clock
    -- when it was earned, as habit check-ins will be. Stored rather than
    -- worked out from `at` later, so moving timezone doesn't rewrite history.
    day        TEXT NOT NULL CHECK (date(day, '+0 days') IS day),
    at         TEXT NOT NULL,
    -- A todo is closed once, so it awards once.
    UNIQUE (source, source_id)
) STRICT;

CREATE INDEX points_by_chat_day ON points (chat_id, day);
CREATE INDEX points_by_user_day ON points (user_id, day);

-- Reports get a third feature, 'points' (the Sunday weekly summary). A
-- CHECK can't be changed in place, so both report tables are rebuilt, as
-- `0009` did for `todos`. Nothing has a foreign key to either.
CREATE TABLE report_settings_new (
    chat_id   INTEGER NOT NULL REFERENCES chats(id),
    feature   TEXT NOT NULL CHECK (feature IN ('prayers', 'habits', 'points')),
    delivery  TEXT NOT NULL CHECK (delivery IN ('here', 'dm', 'off')),
    PRIMARY KEY (chat_id, feature)
) STRICT;
INSERT INTO report_settings_new SELECT chat_id, feature, delivery FROM report_settings;
DROP TABLE report_settings;
ALTER TABLE report_settings_new RENAME TO report_settings;

CREATE TABLE reports_sent_new (
    id          INTEGER PRIMARY KEY,
    chat_id     INTEGER NOT NULL REFERENCES chats(id),
    feature     TEXT NOT NULL CHECK (feature IN ('prayers', 'habits', 'points')),
    kind        TEXT NOT NULL CHECK (kind IN ('day', 'week', 'month')),
    period_end  TEXT NOT NULL CHECK (date(period_end) IS period_end),
    place       TEXT NOT NULL DEFAULT '',
    sent_at     TEXT NOT NULL,
    UNIQUE (chat_id, feature, kind, period_end, place)
) STRICT;
INSERT INTO reports_sent_new (id, chat_id, feature, kind, period_end, place, sent_at)
SELECT id, chat_id, feature, kind, period_end, place, sent_at FROM reports_sent;
DROP TABLE reports_sent;
ALTER TABLE reports_sent_new RENAME TO reports_sent;
