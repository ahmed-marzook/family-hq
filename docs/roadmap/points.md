# Points (gamifying todos)

> **Goal:** finishing todos and praying earns points, so each person can see how their day and week went, with a leaderboard in each group. Habits will earn points the same way later.
> **Status:** Brought forward at the owner's request (2026-11-06). P1 (ledger, points for todos, `/points`), P2 (Sunday weekly summary) and P3 (points for prayers) built; waiting on a week of family use. P4 (weekly reset, history over weeks, months and years) planned.
> **Last updated:** 2026-11-10

## Rules

Whoever closes a todo (`closed_by`) earns its points, worked out by
`points::for_todo` (a pure function, with the most tests in this area):

| Closed as                      | Points                             |
| ------------------------------ | ---------------------------------- |
| Done, 🔴 high                  | 3                                  |
| Done, medium or no priority    | 2                                  |
| Done, 🟢 low                   | 1                                  |
| Done on or before its due date | +1                                 |
| Done after its due date        | −1, but never less than 1 point    |
| Skipped                        | −1                                 |
| Removed                        | 0 (no ledger row)                  |

"On time" is judged on the **closer's own local day**, the same clock
`-d fri` is resolved on.

Decisions:

- **A ledger, not a count.** `points` has one row per award, written in the
  same transaction as closing the todo (the webhook runs each command in
  one). Points aren't worked out from `todos`, because `/todo clear done`
  deletes closed todos and the points must outlive them.
  `UNIQUE (source, source_id)`: a todo awards once.
- **Each row stores the earner's local `day`**, as habit check-ins will, so
  moving timezone doesn't rewrite history.
- **Groups are ranked** 🥇🥈🥉 (the owner's choice; habits stay "together,
  not ranked"). Ties share a place (1, 1, 3). A group's leaderboard counts
  only points earned in that group.
- **A DM's `/points` shows the person's own points from every chat.**
- `source` already allows `'habit'` and `'goal'`, for Stage 2.5.

Known gaps, accepted for now:

- Adding a todo and ticking it straight off earns points. If that becomes
  a problem: a todo closed by its creator within 10 minutes earns at most 1.
- `remove` dodges a skip's −1. Accepted: removing is for mistakes.
- The table grows by one row per closed todo and per prayer: roughly
  20,000 rows (a few MB) a year for a busy family. Kept **forever**,
  because P4's history goes back years; the reads are a week, a year or a
  few years for one person, through `points_by_user_day`. Not a worry. (An
  earlier idea, rolling old rows into a `points_weekly` table, is dropped:
  it would lose the per-day detail that personal bests and year reviews
  need.)

## Stage P1: the ledger, points for todos, `/points` (built)

- Migration `0010_points.sql`: the `points` table, and `report_settings` /
  `reports_sent` rebuilt so their `feature` CHECK allows `'points'` (ready
  for P2).
- `src/points/mod.rs`: the rules, `rank`, `medal`, and the `/points` text.
  `src/repo/points.rs`: `award`, `totals`, `by_day`, `by_chat`.
- `todos::complete` / `skip` return a `ClosedTodo` (title, priority, due).
  `handlers::close_todo` closes a todo and records its points; the reply
  says what it earned: `✅ Done: *fix the tap* · +4 ⭐`, `⏭️ Skipped: *gym* · −1 ⭐`.
- `/points` (`/pts`): in a group, this week's leaderboard (Monday to
  Sunday) and today's earners; in a DM, today, this week, last week, a bar
  for each of the last 7 days, and the split by chat.
- `/help points`; `command_log` name `points`; cleanup deletes a person's
  points with their data and a chat's with the chat.

New concepts: `ON CONFLICT DO NOTHING`, returning several columns from
`UPDATE … RETURNING`, lifetimes on a struct that borrows (`Ranked<'a>`),
sorting with `cmp` and `then_with`.

Done when:

- [x] Closing a todo by reply, `+done` or `+skip` writes one ledger row,
      and the reply shows the points
- [x] `/todo clear done` leaves the points in place
- [x] `/points` ranks a group's week, ties included, and a DM shows that
      person's own days
- [x] `points::for_todo` has tests for every row of the rules table,
      including a month and year end
- [ ] Used by the family for a week, and the rules feel fair

## Stage P2: Sunday weekly summary (built)

Built on `src/reports.rs`, as the prayer reports are:

- `Feature::Points` (`"points"`).
- `src/points/reports.rs`: `check(state, now)`, run every minute from the
  loop in `reminders.rs`. It looks at groups the bot is still in that
  earned points in the last 8 days (`repo::points::report_groups`), and
  sends at **Sunday 20:00 on the group's own clock** (`SUMMARY_HOUR`):
  evening rather than midnight, so nobody is woken by it. Points earned
  later that evening still count in the week, but come after the summary.
  It keeps only `ReportKind::Summary(Span::Week)` from `reports::due` and
  claims with `place = ''`.
- **Groups only.** In a DM, `/points` shows your own week any time, and
  `/points reports` there says so.
- Message (`points::week_text`): the week's leaderboard with medals, each
  person's change from last week (⬆️ 6 / ⬇️ 4, only for someone who earned
  something last week), and 🌟 the best single day. Nothing is sent in a
  week when nobody earned anything. `messages(...) -> Vec<Outgoing>` is a
  pure function, like `prayer::tracking::messages`.
- `/points reports here|dm|off` (`BotCommand::PointsReports`). With `dm`,
  each person who earned points gets only their own week
  (`points::own_week_text`), if they have a DM with the bot.
- Admin: `reports.points` in the API, a "Points" section on the Group page
  to change it, and `POST /api/users/{id}/reports/points` is a 404.

Done when:

- [x] The summary goes once on Sunday evening in the group's timezone, and
      after a restart within `LATE_REPORT_GRACE` (tests in
      `points::reports`, including BST/GMT and Colombo)
- [x] `here` / `dm` / `off` are respected

## Stage P3: points for prayers (built)

At the owner's request (2026-11-08): every successful prayer earns points,
like a todo.

| Prayer           | Points |
| ---------------- | ------ |
| Prayed on time ✅ | 2      |
| Made up 🕐        | 1      |
| Missed ❌         | 0      |

- `points::for_prayer(status)`, a pure function, with
  `PRAYED_POINTS` / `MADE_UP_POINTS`.
- **The points follow the prayer log.** Unlike a todo (closed once), a
  prayer changes: made up, corrected (`+prayed asr on time`), unticked,
  set on the dashboard. So every change goes through `checkin::apply`,
  which calls `sync_points` in the same transaction: the row is added,
  changed or deleted to match. No path can forget.
- **One row per person, prayer and day.** Migration `0011_prayer_points.sql`
  rebuilds `points` with `source = 'prayer'`, reasons `prayed` / `made_up`,
  and two *partial* unique indexes: `(source, source_id)` for todos as
  before, and `(user_id, day, source_id)` for prayers, where `source_id`
  is the prayer (`Prayer::number`, 1 Fajr … 5 Isha) and `day` is the
  prayer's own date (a made-up Isha counts on its day).
- **Which chat:** where it was first ticked. A reaction counts in the chat
  the reminder was in, `+prayed` in the chat it was sent in, a push button
  (and the dashboard) in the person's DM. A later change keeps the chat.
  If the dashboard sets a prayer for someone with no DM and no row yet,
  nothing is added.
- The reaction reply shows it: `✅ *Asr* prayed · +2 ⭐ · ■■■■■■□□□□ 3/5 prayed`.
- `/help points` and the empty `/points` text mention prayers.

Known gaps, accepted for now:

- Prayers can earn up to 10 a day, more than most people's todos. If the
  leaderboard ends up being all prayers, lower `PRAYED_POINTS`.
- Prayed in a group and corrected in a DM: the points stay in the group.

Done when:

- [x] A tick, a correction, an untick and a dashboard change each leave
      the ledger matching the log (`checkin::a_prayers_points_follow_the_log`)
- [x] A retried reaction doesn't pay twice
- [x] Existing todo points survive the migration
- [ ] Used by the family for a week, and the numbers feel fair

## Stage P4: a weekly game, and a history to improve against (planned)

At the owner's request (2026-11-10): the total should start again every
week, but each person should also see how they're doing over weeks,
months and years, to improve on **their own** past rather than only
against each other.

### Two different things

| | The weekly game | Your history |
| --- | --- | --- |
| What | This week's points, everyone from 0 on Monday | Every week, month and year you've had |
| Where | The group (`/points`, Sunday summary) | Your DM with the bot only (private) |
| Compared with | Each other: 🥇🥈🥉 | Yourself: your average, your best, last year |
| Resets | Monday 00:00 on the reader's clock | Never |

**Nothing is deleted to reset.** The ledger keeps every row; "this week"
is only the Monday–Sunday window a query reads (`Period::of(Span::Week)`),
which is how `/points` already works. So the reset can't lose history,
and history needs no new table.

### The reset, made visible

- The Sunday summary ends with "Scores start again tomorrow. 🏁"
- `/points` in a group on a Monday with nothing earned yet says "New week:
  everyone's on 0. Last week: 🥇 Sara 34" instead of the empty text.

### History (DM only)

- `/points weeks`: the last 12 weeks as bars, this week marked, with your
  12-week average and your best week ever.
  `This week 31 · average 26 ⬆️ · best 44 (w/c 5 Oct)`
- `/points months`: the last 12 months as bars, and each one against the
  same month last year once there's a year of data.
- `/points year` (`/points 2026`): the year's total, best week, best month,
  and the split by what earned it (todos / prayers, from `points.source`).
- `/points` in a DM (today, this week, last week, 7 days) gains one line:
  how this week compares with your 4-week average.
- Personal bests are announced once, in the reply that sets them: "🌟 Your
  best week ever (45)". Checked against the ledger, so nothing extra is
  stored.

### Monthly and yearly reviews

- **Monthly, in your DM** on the 1st at 20:00 your time: last month's total
  against the month before and the same month last year, best week, best
  day, and your prayer record (on time / made up / missed, from
  `prayer_log`, which is more telling than points). Built on
  `reports.rs`, which already has `Span::Month`; opt out with
  `/points reports off` in the DM.
- **Year in review**, in your DM on 1 January: the year's total, each
  month as a bar, best month and week, and the year before if there is
  one. Needs `Span::Year` in `reports.rs` (and its `kind` CHECK: a
  migration).

### How it's built

- One read, `repo::points::by_day(user, period)` (it exists), for up to a
  year: at most 366 rows. Grouping into weeks and months happens in Rust
  with pure functions (`points::by_week`, `by_month`, `best`, `average`),
  so it's all testable without a database, like `for_todo`.
- Weeks are Monday–Sunday and months calendar months, on the person's own
  clock, the same as everywhere else. A week that crosses a month (or a
  year) counts in the month of its own days, day by day.
- A week or month with nothing is a 0 bar, not left out, so a gap shows.
- Averages count only weeks since the person's first point, so a newcomer
  isn't dragged down by weeks before they joined.
- Admin dashboard (later, optional): a points-per-week chart per person.

New concepts: iterator adapters for grouping (`fold`, `chunk_by`),
`Option` for "no data yet" in averages, `Span::Year` as a new enum
variant (and how the compiler points out every `match` that must handle it).

Done when:

- [ ] Group totals start from 0 each Monday on each reader's clock (already
      true; tested across a BST change)
- [ ] `/points weeks`, `months` and `year` work in a DM and say so in a group
- [ ] Grouping, averages and bests are pure functions with tests at month
      and year ends, and for someone with less than a week of data
- [ ] The monthly DM review goes once on the 1st, and the year review on
      1 January
