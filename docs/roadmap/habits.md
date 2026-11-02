# Habits and group goals (Stage 2.5)

> **Goal:** daily habits for each person, habits a whole group keeps together, and goals a group works towards as one total.
> **Status:** Brought forward at the owner's request (2026-11-02); planned, built as H1–H6 in order. Not started.
> **Last updated:** 2026-11-02

### Stage 2.5: Habits and group goals (brought forward)

Daily habits for each person, habits a whole group keeps together, and goals
a group works towards as one total. Built in stages H1–H6, in order.

Three shapes:

| Shape              | Where             | Example                                | What's tracked                                                                                  |
| ------------------ | ----------------- | -------------------------------------- | ----------------------------------------------------------------------------------------------- |
| **Personal habit** | A DM with the bot | "Read 10 pages", "No sugar"            | Your own check-ins and streak, private to you                                                   |
| **Group habit**    | A group           | "Quran every day", "Walk after dinner" | Each member checks in for themselves: a streak each, plus a group streak (days everyone did it) |
| **Group goal**     | A group           | "Walk 100 km in November"              | One shared total that everyone adds to, with an end date                                        |

A habit is **build** (do it) or **quit** (don't). A quit habit's streak is
the days since the last slip.

Decisions:

- **A habit or goal is tracked only in the chat it belongs to**, like every
  list. A DM habit can't count towards a group goal, and you check in to a
  group habit in the group. (Considered and dropped: logging in a DM
  towards a group's goal. Matching names across chats isn't worth it.)
- **Everyone in a group takes part in its habits by default**, including
  people who join the group later. Anyone can `leave` a habit and `join`
  it again. So the schema stores **opt-outs**, not members: a habit's
  people are the group's `chat_members` minus its opt-outs. In a DM, the
  only person is the owner.
- **Missed days are visible in the group by default.** Each person can make
  theirs private with a setting in their own DM (`/settings`). Private
  means: they aren't named in the group's "still to go" lines or the
  weekly summary's misses, and their nudge comes to their DM instead (if
  they have one with the bot). Their ✅ and streak still show. A streak
  that resets says as much, and that's accepted.
- **The local day is stored with each check-in**, worked out in **that
  person's own timezone** (their DM settings, as prayers do), next to the
  UTC time. A streak is counted in local days, and recomputing old days
  from UTC after someone moves timezone would rewrite their history.
- **The group streak counts the people taking part now.** Someone who
  leaves stops holding it back, and someone who never checks in should be
  told how to `leave`. (Keeping a history of who took part on which day was
  more than it's worth.)
- **Streaks are pure functions** (`habits::streak`, `group_streak`): the
  check-in days, the schedule and `today` in, a number out. Like
  `parser.rs`, they get the most tests in this stage.
- **Privacy in the admin dashboard:** a DM's habits are counts only, as for
  lists.

Commands (`+did`, not `+d`: `+d` is todo's `+done`):

| Message                                             | Effect                                                                                        |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `+habit read 10 pages` (`+hb`)                      | A daily build habit. In a DM it's yours; in a group it's the group's, and everyone takes part |
| `+habit gym -3/week` / `-days mon,wed,fri`          | Three times a week (streak in weeks) / only on those days                                     |
| `+habit water -8`                                   | A count habit: done once 8 are logged that day                                                |
| `+habit read -at 21:00`                             | A reminder at 9pm, each person's local time                                                   |
| `+habit quit smoking`                               | A quit habit                                                                                  |
| `/habits` (`/hb`)                                   | Today's numbered list: ⬜/✅, 🔥 streak; in a group, who's still to go                        |
| reply `1 3` to `/habits`, or `+did read` / `+did 2` | Check in (`+did water 3` adds 3)                                                              |
| reply `skip 2`                                      | Ill or travelling: keeps the streak but doesn't add to it                                     |
| reply `leave 2` / `join 2` / `remove 2`             | Stop or start taking part / delete the habit                                                  |
| `+slip smoking`                                     | A slip on a quit habit; asks "What got in the way?"                                           |
| `+goal walk 100km by 30-11`                         | A group goal (groups only)                                                                    |
| `+did walk 3`                                       | Adds 3 km to the goal                                                                         |
| `/goals`                                            | Each goal with a progress bar (`progress.rs`) and each person's share                         |

Schema (one migration per stage that needs it, starting at `0007`):

- `habits`: `chat_id`, `created_by`, `name`, `kind` (`build`/`quit`),
  `target` (per day, default 1), `unit`, the schedule (daily, a weekday
  mask, or times per week), `remind_at` (local `HH:MM`), `created_at`,
  `archived_at`. A habit's name is unique in its chat.
- `habit_opt_outs`: `habit_id`, `user_id`, `at`.
- `habit_checkins`: `habit_id`, `user_id`, `day` (local date), `amount`,
  `status` (`done`/`skipped`/`slipped`), `at` (UTC), `obstacle`, `note`.
- `goals`: `chat_id`, `name`, `target`, `unit`, `ends_on`, `created_by`;
  `goal_contributions`: `goal_id`, `user_id`, `amount`, `at`.
- Cleanup (Stage 1.9) deletes a person's check-ins and opt-outs with the
  rest of their data, and a chat's habits and goals with the chat.

#### Stage H1: Personal habits in a DM

- `+habit`, `/habits`, ticking by reply and `+did`, `remove`. Daily yes/no
  build habits only.
- `habits::streak` as a pure function: today not done _yet_ doesn't break
  it; a day off the schedule is skipped over.
- 🎉 at 7, 30 and 100 days.

New concepts: `chrono::NaiveDate` and date arithmetic, an enum stored as
text (`#[derive(sqlx::Type)]`), testing with a fake `today`.

Done when:

- [ ] `+habit`, `/habits`, a reply `1 3` and `+did` work in a DM
- [ ] Streaks are right across a missed day, a month end and a clock change
- [ ] A check-in at 23:30 in London lands on that London day

#### Stage H2: Group habits

- `+habit` in a group: everyone takes part; `leave` / `join`.
- `/habits` in a group: each habit's ✅ count, who's still to go, each
  person's streak and the group streak.
- A check-in replies in the group: "✅ @Ahmed read Quran 🔥 12".
- `/habits` in a DM also lists the groups' habits you take part in, read
  only ("Family: Quran 🔥 5").
- The private-misses setting, in a person's DM settings.

Done when:

- [ ] Someone who joins the group later takes part without doing anything
- [ ] `leave` drops them from the list and the group streak; `join` adds
      them back
- [ ] With misses private, they're never named as "still to go"

#### Stage H3: Habit reminders

- First, the shared reminder engine (the "One reminder engine" bullet of
  Stage 2.2), so habits don't add a third copy of the sending loop. Prayer
  and todo reminders move onto it unchanged.
- A reminder at `remind_at` in each person's local time. In a group it
  @mentions only those who haven't checked in (private people get theirs
  by DM).
- Any reaction on a habit reminder checks in; ❌ or 👎 is a miss (H4).

Done when:

- [ ] A habit reminder fires at the right local time, once, and catches up
      after a restart
- [ ] Reacting to it checks in
- [ ] Prayer and todo reminders behave as before

#### Stage H4: Quit habits and obstacles

- `+habit quit …`, `+slip`. The streak is days since the last slip.
- A slip, a ❌/👎 reaction, or a miss → "What got in the way?" with
  `1 Work  2 Sleep  3 Procrastination  4 Travel`. The reply is stored as an
  obstacle plus an optional note.
- `/status`: your streaks and your most common obstacle this week.

Done when:

- [ ] A week of check-ins and slips produces a sensible `/status`

#### Stage H5: Counts, weekly habits and group goals

- Count habits (`-8`), `-3/week`, `-days mon,wed,fri`.
- `+goal`, `+did walk 3`, `/goals` with progress bars and each person's
  share. A message in the group when a goal is reached, and when it ends
  unmet.

Done when:

- [ ] A count habit is done only at its target, and `-3/week` streaks
      count weeks
- [ ] A goal's total is the sum of everyone's `+did`, and it can't be added
      to after its end date

#### Stage H6: Weekly summary and the admin dashboard

- Sunday evening, in each group with habits: everyone's streaks, the group
  streak, goal progress, and the most common obstacle. Together, not ranked.
- Admin dashboard: habit and goal counts per person and group (DMs counts
  only); `command_log` names for the new commands.

Done when:

- [ ] The summary goes once a week, in the group's timezone
- [ ] No DM habit's name reaches the admin API
