-- Stats and levels (Stage P4): each person's highest level per category,
-- and Overall, so a level-up is announced once. Levels themselves aren't
-- stored: they're worked out from the `points` ledger every time
-- (`points::levels`). This only remembers what has been announced, so
-- dropping below a level (a prayer unticked) and earning it back isn't
-- announced again.
CREATE TABLE levels_reached (
    user_id    INTEGER NOT NULL REFERENCES users(id),
    -- A category, or 'overall' (the sum of the category levels). 'habits'
    -- is for Stage 2.5, allowed already so it needs no table rebuild.
    category   TEXT NOT NULL
               CHECK (category IN ('prayers', 'todos', 'habits', 'overall')),
    -- The highest level reached. Everyone starts at 1.
    level      INTEGER NOT NULL CHECK (level >= 1),
    reached_at TEXT NOT NULL,
    PRIMARY KEY (user_id, category)
) STRICT;

-- People who already have points start at the levels they've reached, so
-- their first award after this doesn't announce weeks of old level-ups.
--
-- A snapshot of the rules at the time of writing, as a migration must be:
-- XP is the sum of positive awards, the paces are 35 for prayers and 15
-- for todos (`Category::pace`), and level L takes pace × L × (L − 1) / 2.
-- The highest such L is found by trying levels 1 to 500 (todo level 500
-- would be nearly 2 million XP).
WITH RECURSIVE
    n(level) AS (SELECT 1 UNION ALL SELECT level + 1 FROM n WHERE level < 500),
    category(name, source, pace) AS (VALUES ('prayers', 'prayer', 35), ('todos', 'todo', 15)),
    xp(user_id, name, pace, xp) AS (
        SELECT e.user_id, c.name, c.pace,
               (SELECT COALESCE(SUM(p.points), 0) FROM points p
                WHERE p.user_id = e.user_id AND p.source = c.source AND p.points > 0)
        FROM (SELECT DISTINCT user_id FROM points) e
        CROSS JOIN category c
    )
INSERT INTO levels_reached (user_id, category, level, reached_at)
SELECT x.user_id, x.name, MAX(n.level), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM xp x
JOIN n ON x.pace * n.level * (n.level - 1) / 2 <= x.xp
GROUP BY x.user_id, x.name;

INSERT INTO levels_reached (user_id, category, level, reached_at)
SELECT user_id, 'overall', SUM(level), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM levels_reached
GROUP BY user_id;
