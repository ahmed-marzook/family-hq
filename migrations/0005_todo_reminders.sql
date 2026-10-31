-- Who has asked for todo reminders (`/todo reminders on`), per person per
-- chat. No row means off, which is everyone's default.
CREATE TABLE todo_reminders (
    chat_id       INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The newest reminder time already dealt with (UTC), so each one goes
    -- out once. Starts as the moment it was turned on, so turning it on at
    -- 13:10 doesn't immediately send the 13:00 one.
    last_slot_at  TEXT NOT NULL,
    PRIMARY KEY (chat_id, user_id)
) STRICT;
