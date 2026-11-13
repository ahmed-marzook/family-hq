-- Personal bests (Stage P4, part 3): "🌟 Your best week ever" is announced
-- once, in the reply that sets it. The bests themselves aren't stored:
-- they're worked out from the `points` ledger (`points::history`). This
-- only remembers which week or month was last announced as a best, so
-- every later point that same week doesn't announce it again.
CREATE TABLE bests_announced (
    user_id      INTEGER NOT NULL REFERENCES users(id),
    span         TEXT NOT NULL CHECK (span IN ('week', 'month')),
    -- The first day of the week (a Monday) or month that was announced,
    -- on the person's own clock.
    period_first TEXT NOT NULL CHECK (date(period_first) IS period_first),
    announced_at TEXT NOT NULL,
    PRIMARY KEY (user_id, span)
) STRICT;
