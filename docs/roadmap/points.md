# Points (gamifying todos)

> **Goal:** finishing todos and praying earns points, so each person can see how their day and week went, with a leaderboard in each group. Habits will earn points the same way later.
> **Status:** Brought forward at the owner's request (2026-11-06). P1 (ledger, points for todos, `/points`), P2 (Sunday weekly summary) and P3 (points for prayers) built; waiting on a week of family use. P4 in progress: part 1 (stats and levels) built; weekly champions, history and reviews planned.
> **Last updated:** 2026-11-13

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

## Stage P4: stats, levels, a weekly game and a history (in progress)

**Part 1 (stats and levels) is built** (2026-11-13). How it's laid out,
so parts 2–4 and habits reuse it:

- `src/points/levels.rs`, pure: `Category` (`of`, `pace`, `emoji`,
  `ALL`), `Track` (each category plus Overall, with its `start` level),
  `level`, `xp_to_reach`, `overall`, `by_category`, `Stats` (its
  `Display` is the stats card), `overall_by_person`, `level_ups` and
  `level_up_text`.
- `src/points/level_ups.rs`, the glue: `stats(user)`, `announce(user)`,
  and `lines` / `for_sender` for a reply. **Announcing is separate from
  awarding**: any reply to someone who may have earned something calls
  `for_sender` once at the end (todo replies, `+prayed`, a prayer
  reaction). A level reached where there's no reply (a push button, the
  dashboard) is announced in that person's next one.
- `repo::points::xp_by_source`, `xp_of_earners`, `by_source`, and
  `repo::levels` (`reached`, `record`, never lowering a level).
- Migration `0012_levels_reached.sql` fills in everyone's current levels,
  so nobody gets weeks of old level-ups at once. With no row, a track
  counts from its `start` (Lv 1; Overall one per category).
- Habits: add a `Category` variant and a `Track` db string; the compiler
  points at the rest, and the CHECK already allows `'habits'`.

At the owner's request (2026-11-10): points should work like game stats.
Each feature (prayers, todos, later habits) has its own category with its
own XP and level, and the categories add up to an **Overall** level. That
lifetime progress never resets. Next to it, the group game starts from 0
every week. And each person gets a private history, so they can improve
on their own past, not only against each other.

### The three views of the same ledger

| | Stats and levels | The weekly game | Your history |
| --- | --- | --- | --- |
| What | Lifetime XP and a level per category, plus Overall | This week's points, everyone from 0 on Monday | Every week, month and year you've had |
| Where | Everywhere: your DM, and next to your name in groups | The group (`/points`, Sunday summary) | Your DM only (private) |
| Compared with | Your next level | Each other: 🥇🥈🥉, and a champion per category | Yourself: your average, your best, last year |
| Resets | Never | Monday 00:00 on the reader's clock | Never |

**Nothing new is stored for any of them** except the levels people have
reached (for announcements, below). The ledger keeps every row with its
`source` and `day`: lifetime XP is a sum, a category is `GROUP BY source`,
"this week" is the Monday–Sunday window a query reads (as `/points` does
now), and history is the same rows grouped by week or month.

### Categories

| Category | Sources       | Pace (XP per level step) |
| -------- | ------------- | ------------------------ |
| 🕌 Prayers | `prayer`      | 35                       |
| 📝 Todos   | `todo`        | 15                       |
| 🌱 Habits  | `habit`, `goal` | decided in H1          |

- `Category::of(Source)` maps each source to a category. Group goals count
  under Habits rather than having a category of their own.
- A new feature becomes a new `Category` variant, and the compiler then
  points at every `match` that has to handle it.

### Levels

- Going from level L to L+1 costs `pace × L` XP, so reaching level L takes
  `pace × L × (L − 1) / 2` in total. Early levels come quickly, later ones
  take real effort. Everyone starts at level 1.
- The paces are set so a steady week moves each category at about the same
  speed. A steady person earns about 65 prayer XP and 25 todo XP a week,
  which gives roughly:

  | Level | Prayers XP | Todos XP | Roughly when |
  | ----- | ---------- | -------- | ------------ |
  | 2     | 35         | 15       | first few days |
  | 5     | 350        | 150      | ~6 weeks     |
  | 10    | 1,575      | 675      | ~6 months    |
  | 15    | 3,675      | 1,575    | ~14 months   |

  Tune the paces after a few weeks of real data. A change only moves
  levels, never the XP itself.
- **Overall = the sum of the category levels** (option B). It rewards
  being well-rounded rather than maxing one category, and stays balanced
  when habits arrive. When they do, everyone gains a Habits Lv 1 and their
  Overall goes up by one: announce it ("🌱 Habits are here").
- **XP only counts what was earned.** A skipped todo's −1 lowers that
  week's points, but not XP: lifetime progress isn't lost for being honest
  about a skip. XP only drops when an earned award is undone (a prayer
  unticked, a mistake corrected), which is right, since it shouldn't have
  counted.
- **Level-ups are announced once.** A table, `levels_reached (user_id,
  category, level, reached_at)`, holds each person's highest level per
  category. A level above it gets a line added to the reply that earned
  it (`🎉 Prayers level 6!`), and the row is updated in the same
  transaction. If an untick drops someone below a level and they earn it
  back, it isn't announced again. Overall level-ups are announced too, as
  a separate line (`🏅 Overall level 12`).
- XP and levels are **lifetime and personal**: XP from every chat counts.
  The weekly leaderboard stays per group, as now.

### What people see

`/points` in a DM:

```
📊 Your stats · Overall Lv 10
🕌 Prayers  Lv 6 ■■■■■■□□□□ 650 / 735
📝 Todos    Lv 4 ■■■■■□□□□□ 120 / 150
This week: 31 (🕌 18 · 📝 13) · Last week: 26
```

(Overall 10 = 6 + 4. Each bar is the way through the current level:
Prayers 6 starts at 525 XP and 7 at 735. Today, the 7-day bars and the
split by chat stay below it.)

`/points` in a group, with each person's Overall level:

```
🥇 Sara · Lv 9 · 34
🥈 Bilal · Lv 7 · 31
```

### The weekly game

- Weekly points stay the raw sum of points earned in the group, as now.
  Prayers can outweigh todos there, so the **category champions** balance
  it: the Sunday summary names the top earner in each category as well as
  the overall winner (`🕌 Most prayers: Khadija · 📝 Most todos: Bilal ·
  🏆 Overall: Sara`). More people can win something each week.
- **The reset is made visible:** the Sunday summary ends with "Scores start
  again tomorrow. 🏁", and on a Monday with nothing earned yet, `/points`
  in a group shows last week's winner instead of the empty text.

### History (DM only)

- `/points weeks`: the last 12 weeks as bars, this week marked, with your
  12-week average and your best week ever.
  `This week 31 · average 26 ⬆️ · best 44 (w/c 5 Oct)`
- `/points months`: the last 12 months as bars, each against the same month
  last year once there's a year of data.
- `/points year` (`/points 2026`): the year's total, best week, best month,
  and the levels gained that year.
- Each of them per category too: "Prayer XP up 20% this month, todos flat."
- A personal best is announced once, in the reply that sets it ("🌟 Your
  best week ever (45)"), worked out from the ledger.

### Monthly and yearly reviews

- **Monthly, in your DM** on the 1st at 20:00 your time: last month's total
  and each category against the month before and the same month last year,
  levels gained, best week, best day, and your prayer record (on time /
  made up / missed, from `prayer_log`). Built on `reports.rs`, which
  already has `Span::Month`; opt out with `/points reports off` in the DM.
- **Year in review**, in your DM on 1 January: the year's XP per category,
  levels gained, each month as a bar, best month and week. Needs
  `Span::Year` in `reports.rs` and its `kind` CHECK (a migration).

### How it's built

- New reads in `repo::points`: `xp_by_source(user)` (lifetime XP per
  source, only positive awards), `by_day_and_source(user, period)` for
  history, and the group week by person and source for the champions.
  History reads at most a year for one person: a few thousand rows
  through `points_by_user_day`.
- Pure functions in `points` (no database, most of the tests):
  `Category::of`, `level(xp, pace) -> Level { level, into, needed }`,
  `overall(&levels)`, `by_week`, `by_month`, `average`, `best`,
  `champions`.
- Weeks are Monday–Sunday and months calendar months, on the person's own
  clock, as everywhere else. A week or month with nothing is a 0 bar, not
  left out. Averages only count weeks since the person's first point.
- Admin dashboard (optional, later): a chart of XP per week per person.

New concepts: a `Category` enum with a `const` table of paces,
`impl Display` for the stats card, `BTreeMap<Category, i64>` for a stable
order, iterator grouping (`fold`, `chunk_by`), and `Option` for "no data
yet" in averages.

### Order

1. **Stats and levels**: categories, `level`, `overall`, the stats card in
   `/points`, Lv next to names, `levels_reached` and the level-up lines.
2. **The weekly game**: category champions in the Sunday summary and the
   visible reset.
3. **History**: `/points weeks`, `months`, `year`, and personal bests.
4. **Reviews**: monthly in the DM, then the year in review.

Done when:

- [x] `level` and `overall` have tests at each level boundary, for 0 XP,
      and for XP that drops below a level
- [x] Each level-up is announced once, even after an untick and a re-tick
- [x] A skip lowers the week's points but not XP
- [ ] The Sunday summary names a champion per category
- [ ] Group totals start from 0 each Monday on each reader's clock (tested
      across a BST change)
- [ ] `/points weeks`, `months` and `year` work in a DM and say so in a
      group; grouping, averages and bests are tested at month and year
      ends, and for someone with less than a week of data
- [ ] The monthly DM review goes once on the 1st, and the year review on
      1 January
