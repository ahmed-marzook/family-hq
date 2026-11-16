-- Jot J3: chosen days, a pause, and prompts answered with `skip`.

-- Which weekdays prompts go out on: a bit per day (Monday = 1 ... Sunday =
-- 64), so 127 is every day, the default.
ALTER TABLE jot_settings ADD COLUMN days INTEGER NOT NULL DEFAULT 127
    CHECK (days BETWEEN 1 AND 127);

-- No prompts until this moment (UTC), without turning Jot off. NULL or in
-- the past means not paused, so a pause ends by itself.
ALTER TABLE jot_settings ADD COLUMN paused_until TEXT;

-- When the person answered the prompt with `skip`. A skipped slot counts as
-- neither logged nor missed.
ALTER TABLE jot_prompts ADD COLUMN skipped_at TEXT;
