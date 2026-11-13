-- Reports get a fourth kind, 'year': the year in review sent to each
-- person's DM on 1 January (Stage P4, part 4). A CHECK can't be changed in
-- place, so `reports_sent` is rebuilt, as 0010 did. Nothing has a foreign
-- key to it.
CREATE TABLE reports_sent_new (
    id          INTEGER PRIMARY KEY,
    chat_id     INTEGER NOT NULL REFERENCES chats(id),
    feature     TEXT NOT NULL CHECK (feature IN ('prayers', 'habits', 'points')),
    kind        TEXT NOT NULL CHECK (kind IN ('day', 'week', 'month', 'year')),
    period_end  TEXT NOT NULL CHECK (date(period_end) IS period_end),
    place       TEXT NOT NULL DEFAULT '',
    sent_at     TEXT NOT NULL,
    UNIQUE (chat_id, feature, kind, period_end, place)
) STRICT;
INSERT INTO reports_sent_new (id, chat_id, feature, kind, period_end, place, sent_at)
SELECT id, chat_id, feature, kind, period_end, place, sent_at FROM reports_sent;
DROP TABLE reports_sent;
ALTER TABLE reports_sent_new RENAME TO reports_sent;
