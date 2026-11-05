# Usage metrics

Every time someone uses the bot, a row goes into the `command_log` table
(migration `0004`). Ordinary chat in a group is never logged, and neither
is what anyone wrote: a row says `todo.add`, not the todo's title.

When a person's data is cleared for not using the bot (`INACTIVE_DAYS`,
see `src/cleanup.rs`), their rows here are deleted too, and so are a
group's when the bot leaves it: past totals drop.

The admin dashboard's **Metrics** page shows the main ones (for 7 days, 30
days or all time, with days and hours on the UK clock), including which
chats the bot is used in. Each person's page and each group's page has the
same report for just them (`/api/metrics?user=3` or `?chat=5`). The queries
below are for anything it doesn't show.

| Column | Example | Meaning |
|---|---|---|
| `at` | `2026-10-28T18:02:11…+00:00` | When, in UTC |
| `chat_id`, `user_id` | `3`, `1` | Where and who (join `chats`, `users`) |
| `kind` | `command` | `command`, `list_reply` (`done 2` on a list), `reaction` (on a prayer reminder), `unknown` (a `+word`/`/word` the bot didn't understand) |
| `name` | `todo.list` | Which command. For `unknown`, the word typed (`+remind`) |
| `detail` | `by_person` | A fixed word about it: `week`, `numbers`, `someone`, `fajr,asr`... |
| `typed` | `/t` | The first word as typed, to see if short forms get used |
| `is_group` | `1` | Group (1) or DM (0) |
| `ok` | `1` | 0 if it failed (the error is in the server log) |
| `duration_ms` | `180` | Time to work out the answer, before sending it |

Command names: `shop.add`, `shop.list`, `shop.got`, `shop.clear`,
`todo.add`, `todo.list`, `todo.done`, `todo.clear`, `todo.reminders`, `prayers.list`,
`prayers.reminders`, `prayers.summary`, `prayers.reports`, `prayers.prayed`, `prayers.react`, `settings`,
`settings.location`, `weather`, `moon`, `push` (detail `on`, `off`, `status`
or `test`), `help`, and list replies like `todo.reply_done` or
`shop.reply_tick`. Buttons tapped on a push are logged with `kind`
`reaction`: `prayers.push` (Prayed) and `push.test`.

## Opening the database

On the server, once (`-readonly` means a typo can't change anything, and
it's safe while the bot is running):

```sh
sudo apt install sqlite3
sqlite3 -readonly -box /opt/stacks/family-hq/data/family-hq.db
```

Then paste a query below. `.quit` to leave. To run one without the prompt:
`sqlite3 -readonly -box /opt/stacks/family-hq/data/family-hq.db "SELECT ..."`.

Change `-7 days` to `-30 days` (or remove the `WHERE`) for other periods.
Hours are UTC: add one for UK summer time.

## What gets used

Most used commands this week:

```sql
SELECT name, count(*) AS uses
FROM command_log
WHERE kind != 'unknown' AND datetime(at) >= datetime('now', '-7 days')
GROUP BY name
ORDER BY uses DESC;
```

The same, split by the detail (which `/todo` view, which `/weather`...):

```sql
SELECT name, coalesce(detail, '') AS detail, count(*) AS uses
FROM command_log
WHERE kind != 'unknown' AND datetime(at) >= datetime('now', '-7 days')
GROUP BY name, detail
ORDER BY name, uses DESC;
```

Commands nobody has used in 30 days show up by being missing above. First
and last use of each, to see what caught on:

```sql
SELECT name, date(min(at)) AS first_used, date(max(at)) AS last_used, count(*) AS uses
FROM command_log
WHERE kind != 'unknown'
GROUP BY name
ORDER BY last_used DESC;
```

## Who uses what

Per person:

```sql
SELECT u.display_name AS person, count(*) AS uses,
       count(DISTINCT date(l.at)) AS active_days,
       date(max(l.at)) AS last_seen
FROM command_log l JOIN users u ON u.id = l.user_id
WHERE datetime(l.at) >= datetime('now', '-30 days')
GROUP BY u.id
ORDER BY uses DESC;
```

Each person's favourite commands:

```sql
SELECT u.display_name AS person, l.name, count(*) AS uses
FROM command_log l JOIN users u ON u.id = l.user_id
WHERE datetime(l.at) >= datetime('now', '-30 days')
GROUP BY u.id, l.name
ORDER BY person, uses DESC;
```

People who haven't used the bot in a week:

```sql
SELECT u.display_name AS person, date(max(l.at)) AS last_used
FROM users u LEFT JOIN command_log l ON l.user_id = u.id
GROUP BY u.id
HAVING max(l.at) IS NULL OR datetime(max(l.at)) < datetime('now', '-7 days');
```

Groups vs. DMs:

```sql
SELECT CASE is_group WHEN 1 THEN 'group' ELSE 'dm' END AS place, name, count(*) AS uses
FROM command_log
WHERE datetime(at) >= datetime('now', '-30 days')
GROUP BY is_group, name
ORDER BY place, uses DESC;
```

## When

Uses per day:

```sql
SELECT date(at) AS day, count(*) AS uses, count(DISTINCT user_id) AS people
FROM command_log
WHERE datetime(at) >= datetime('now', '-30 days')
GROUP BY day
ORDER BY day;
```

Busiest hours (UTC):

```sql
SELECT strftime('%H', at) AS hour, count(*) AS uses
FROM command_log
GROUP BY hour
ORDER BY hour;
```

## What doesn't work

What people type that the bot doesn't understand. Top entries here are the
best ideas for new commands (or new spellings to accept):

```sql
SELECT name AS typed, count(*) AS times, count(DISTINCT user_id) AS people
FROM command_log
WHERE kind = 'unknown'
GROUP BY name
ORDER BY times DESC;
```

Failures (details are in the server log at that time):

```sql
SELECT l.at, u.display_name AS person, l.name, l.detail
FROM command_log l JOIN users u ON u.id = l.user_id
WHERE l.ok = 0 AND l.kind != 'unknown'
ORDER BY l.at DESC
LIMIT 20;
```

Slowest commands (usually an outside API: AlAdhan, Open-Meteo):

```sql
SELECT name, count(*) AS uses, round(avg(duration_ms)) AS avg_ms, max(duration_ms) AS max_ms
FROM command_log
WHERE kind IN ('command', 'list_reply')
GROUP BY name
ORDER BY avg_ms DESC;
```

## Habits

Are the short forms used? (`typed` is what was typed, `name` what it meant):

```sql
SELECT name, typed, count(*) AS uses
FROM command_log
WHERE typed IS NOT NULL AND kind != 'unknown'
GROUP BY name, typed
ORDER BY name, uses DESC;
```

How prayers get ticked off: reacting to a reminder, typing `+prayed`, by
number from `/prayers`, or the Prayed button on a push:

```sql
SELECT name, coalesce(detail, '') AS detail, count(*) AS times
FROM command_log
WHERE name IN ('prayers.react', 'prayers.prayed', 'prayers.push')
GROUP BY name, detail
ORDER BY times DESC;
```

How todos are finished: by number, by name, or by replying to a list:

```sql
SELECT name, coalesce(detail, '') AS how, count(*) AS times
FROM command_log
WHERE name IN ('todo.done', 'todo.reply_done', 'todo.reply_tick')
GROUP BY name, detail
ORDER BY times DESC;
```

## Size

About 100 bytes a row: a busy family's year is a few MB, so nothing is
deleted. To clear old rows anyway (needs write access, so no `-readonly`):

```sql
DELETE FROM command_log WHERE datetime(at) < datetime('now', '-1 year');
```
