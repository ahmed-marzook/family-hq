# Points (gamifying todos)

> **Goal:** finishing todos earns points, so each person can see how their day and week went, with a leaderboard in each group. Habits will earn points the same way later.
> **Status:** Brought forward at the owner's request (2026-11-06). P1 (ledger, points for todos, `/points`) and P2 (Sunday weekly summary) built; waiting on a week of family use.
> **Last updated:** 2026-11-06

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
- The table grows by one row per closed todo: about 10,000 rows (1–2 MB) a
  year for a busy family, and every query reads a week or two through an
  index. Not a worry. If it ever is: a nightly job that rolls rows older
  than ~6 months into a `points_weekly` table and deletes them (losing
  their per-day detail).

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
