-- Push notifications through ntfy (Stage 2.1). Generic: prayers are the
-- first feature to use them; habits, todos and `+remind` can follow
-- without a new table.

-- People who turned push on (`/push on`). Each has their own ntfy topic,
-- which their phone subscribes to, and a separate token that the buttons on
-- their pushes send back. Both are long random strings, because each is the
-- only thing standing guard: knowing the topic lets you read someone's
-- pushes, and knowing the token lets you press their buttons. Neither is
-- ever sent to the admin dashboard.
CREATE TABLE push_subscriptions (
    user_id     INTEGER PRIMARY KEY REFERENCES users(id),
    topic       TEXT NOT NULL UNIQUE CHECK (length(topic) >= 24),
    token       TEXT NOT NULL UNIQUE CHECK (length(token) >= 32),
    created_at  TEXT NOT NULL
) STRICT;

-- Each push a feature sent someone. Claimed before sending, like
-- `reports_sent`, so a person reminded in their DM *and* in a group gets
-- one push, not two, and a restart never repeats one.
CREATE TABLE pushes_sent (
    id        INTEGER PRIMARY KEY,
    user_id   INTEGER NOT NULL REFERENCES users(id),
    -- Which feature sent it. The later ones are allowed already, so they
    -- need no table rebuild.
    feature   TEXT NOT NULL
              CHECK (feature IN ('prayers', 'habits', 'todos', 'reminders')),
    -- What it was about, in the feature's own words: for prayers
    -- '2026-11-05 asr start'.
    key       TEXT NOT NULL CHECK (key != ''),
    sent_at   TEXT NOT NULL,
    UNIQUE (user_id, feature, key)
) STRICT;
