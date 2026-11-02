-- Missed and made-up prayers, and reports: the daily, weekly and monthly
-- messages that sum up a feature (prayers first, habits later).
--
-- `prayer_log` gains a status. SQLite can't change a table's constraints
-- in place, so (as in 0002) this creates the new shape, copies the rows
-- over, drops the old and renames. Every existing row was a check-in, so
-- they all become 'prayed'.

CREATE TABLE prayer_log_new (
    user_id      INTEGER NOT NULL REFERENCES users(id),
    prayer_date  TEXT NOT NULL CHECK (date(prayer_date) IS prayer_date),
    prayer       TEXT NOT NULL
                 CHECK (prayer IN ('fajr', 'dhuhr', 'asr', 'maghrib', 'isha')),
    -- 'prayed': checked in while its time lasted. 'missed': its time ended
    -- with no check-in, so the bot marked it. 'late': checked in after it
    -- was marked missed (made up, qada).
    status       TEXT NOT NULL CHECK (status IN ('prayed', 'late', 'missed')),
    -- When they checked in. NULL for a missed prayer.
    prayed_at    TEXT,
    -- When the bot marked it missed. NULL if it never was.
    missed_at    TEXT,
    CHECK ((status = 'missed') = (prayed_at IS NULL)),
    CHECK ((status = 'prayed') = (missed_at IS NULL)),
    PRIMARY KEY (user_id, prayer_date, prayer)
) STRICT;

INSERT INTO prayer_log_new (user_id, prayer_date, prayer, status, prayed_at)
SELECT user_id, prayer_date, prayer, 'prayed', prayed_at
FROM prayer_log;

DROP TABLE prayer_log;
ALTER TABLE prayer_log_new RENAME TO prayer_log;

-- Where a chat's reports for one feature go (`/prayers reports here|dm|off`).
-- No row means 'here'.
CREATE TABLE report_settings (
    chat_id   INTEGER NOT NULL REFERENCES chats(id),
    -- Whose reports: 'prayers' now; 'habits' is for Stage 2.5, allowed
    -- already so it needs no table rebuild.
    feature   TEXT NOT NULL CHECK (feature IN ('prayers', 'habits')),
    -- 'here' in the chat itself, 'dm' to each member privately (a group
    -- only), 'off' not at all.
    delivery  TEXT NOT NULL CHECK (delivery IN ('here', 'dm', 'off')),
    PRIMARY KEY (chat_id, feature)
) STRICT;

-- Each report a chat has had: a day's, a week's or a month's, for one
-- feature. Like `prayer_reminders`, a row is claimed before sending, so a
-- restart or a second check can never send one twice.
CREATE TABLE reports_sent (
    id          INTEGER PRIMARY KEY,
    chat_id     INTEGER NOT NULL REFERENCES chats(id),
    feature     TEXT NOT NULL CHECK (feature IN ('prayers', 'habits')),
    kind        TEXT NOT NULL CHECK (kind IN ('day', 'week', 'month')),
    -- The last day the report covers, in the place's timezone.
    period_end  TEXT NOT NULL CHECK (date(period_end) IS period_end),
    -- Which members it's for when a group's are split by place (prayers:
    -- `Location::key`); '' otherwise.
    place       TEXT NOT NULL DEFAULT '',
    sent_at     TEXT NOT NULL,
    UNIQUE (chat_id, feature, kind, period_end, place)
) STRICT;
