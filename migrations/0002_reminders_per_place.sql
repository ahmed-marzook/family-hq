-- Group prayer reminders now follow each member's own settings, so one
-- group can get several reminders for the same prayer: one per place
-- (e.g. one for members in the UK, one for a member in Sri Lanka), each
-- @mentioning the people it's for. `place` tells them apart; it's '' for a
-- DM's single reminder.
--
-- SQLite can't change a UNIQUE constraint in place, so the table is rebuilt:
-- create the new shape, copy the rows over, drop the old, rename.

CREATE TABLE prayer_reminders_new (
    id             INTEGER PRIMARY KEY,
    chat_id        INTEGER NOT NULL REFERENCES chats(id),
    -- The day in the place's timezone ('2026-10-27').
    prayer_date    TEXT NOT NULL CHECK (date(prayer_date) IS prayer_date),
    prayer         TEXT NOT NULL
                   CHECK (prayer IN ('fajr', 'dhuhr', 'asr', 'maghrib', 'isha')),
    -- 'start': the prayer's time has begun. 'ending': it ends soon.
    kind           TEXT NOT NULL CHECK (kind IN ('start', 'ending')),
    -- Which members' times this is for: city, country, method, school and
    -- timezone joined into one key. '' when there's only one (a DM).
    place          TEXT NOT NULL DEFAULT '',
    prayer_time    TEXT NOT NULL,       -- when the prayer starts, UTC
    -- Set once WhatsApp has accepted the message. NULL if sending failed.
    wa_message_id  TEXT UNIQUE,
    sent_at        TEXT NOT NULL,
    -- At most one of each kind per prayer per day per place per chat. The
    -- loop inserts first and only sends if the insert worked, so a restart
    -- can never send the same reminder twice.
    UNIQUE (chat_id, prayer_date, prayer, kind, place)
) STRICT;

INSERT INTO prayer_reminders_new
    (id, chat_id, prayer_date, prayer, kind, place, prayer_time, wa_message_id, sent_at)
SELECT id, chat_id, prayer_date, prayer, kind, '', prayer_time, wa_message_id, sent_at
FROM prayer_reminders;

DROP TABLE prayer_reminders;
ALTER TABLE prayer_reminders_new RENAME TO prayer_reminders;
