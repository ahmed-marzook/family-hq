-- The end-of-day Jot summary, claimed before it's sent so each person gets
-- one per local day even if the loop runs again or the bot restarts.
CREATE TABLE jot_summaries (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    day        TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    PRIMARY KEY (user_id, day)
) STRICT;
