-- Usage metrics: one row each time someone uses the bot, so we can see
-- which commands are used, how often, by whom, and what doesn't work.
--
-- Only *what kind* of thing was done is kept, never the message itself:
-- `name` is the command ('todo.add'), `detail` a fixed word about it
-- ('mention', 'week'), never a todo title, shop item or city. Queries to
-- read it are in docs/METRICS.md.
CREATE TABLE command_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           TEXT NOT NULL,
    chat_id      INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- 'command'    a typed command (+todo, /prayers, a shared location)
    -- 'list_reply' a reply to one of the bot's lists (`done 2`, `1 3`)
    -- 'reaction'   a reaction on a prayer reminder
    -- 'unknown'    a message starting with + or / that the bot didn't
    --              understand (`name` is the word typed, e.g. '+remind')
    kind         TEXT NOT NULL
                 CHECK (kind IN ('command', 'list_reply', 'reaction', 'unknown')),
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 40),
    detail       TEXT CHECK (detail IS NULL OR length(detail) BETWEEN 1 AND 40),
    -- The first word as typed, when it starts with + or / ('/t', '+d'), to
    -- see whether short forms get used. NULL for replies and reactions.
    typed        TEXT CHECK (typed IS NULL OR length(typed) BETWEEN 1 AND 40),
    is_group     INTEGER NOT NULL CHECK (is_group IN (0, 1)),
    -- 0 if handling it failed (the error is in the server log).
    ok           INTEGER NOT NULL CHECK (ok IN (0, 1)),
    -- How long the bot took to work out the answer, not counting sending
    -- it on WhatsApp. Slow ones are usually an outside API.
    duration_ms  INTEGER NOT NULL CHECK (duration_ms >= 0)
) STRICT;

-- Most questions are "in the last N days".
CREATE INDEX command_log_at ON command_log (at);
