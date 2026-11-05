# Reminders & Prayer Coach (Phase 2)

> **Goal:** `+remind`, ntfy pushes, prayer windows, escalation, reflections.
> **Status:** Prayer reminders and check-ins done early; missed tracking and prayer reports done early (2.4); ntfy push built, first deploy to do (2.1); still to do: `+remind`, the shared reminder engine and ticking off by reaction (2.2), the AlAdhan fallback and BST/GMT test (2.3), the `PrayerStage` state machine (2.4).
> **Last updated:** 2026-11-05

## Phase 2: Reminders & Prayer Coach

Starts after Phase 1 has been in daily use for two weeks.

> **Brought forward (done early, WhatsApp only):** most of Stages 2.3 and
> 2.4 already runs. Times come from AlAdhan per settings (city, method, Asr
> school, timezone), cached in memory per location and day. Chats opt in
> with `/prayers on` and get a reminder when each prayer starts and one
> shortly before it ends (`src/reminders.rs`; `prayer_reminders` makes
> sure each is sent once). **In a group, each member is @mentioned on
> their own times** (one message per place), so family in different
> countries get theirs at the right local time. "Ending soon" only
> mentions people who haven't prayed. Any reaction on a reminder, or
> `+prayed asr` / `+p 3`, marks it in `prayer_log`. Still to do:
> yesterday's-times fallback, and ntfy.
>
> **Missed tracking and prayer reports, at the owner's request
> (2026-11-02):** see "Missed prayers and reports" under Stage 2.4.
>
> **Also brought forward, at the owner's request (2026-11-02):** habits and
> group goals, Stage 2.5 below, built as stages H1–H6.

### Stage 2.1: ntfy push notifications (built; first deploy to do)

Built (decided 2026-11-05: Android and iPhone, reached through a
Cloudflare tunnel, push *as well as* WhatsApp, opt-in by anyone):

- An `ntfy` container and a `cloudflared` tunnel (compose profile `push`).
  The tunnel passes only ntfy and family-hq's `/actions/`. ntfy relays a
  wake-up through ntfy.sh for iPhones (a message ID only).
- `/push on|off|test` (DM only). Each person gets a random topic and a
  separate random token (`push_subscriptions`, migration `0008`).
- `src/push.rs` is generic: a feature describes a `Notice` (title, message,
  buttons) and calls `push::send_once`, which claims a `pushes_sent` row
  first (per feature and key, so a DM and a group never push twice). A
  button is a `push::Action` with its own path.
- `POST /actions/{*path}` (`routes::actions`): the phone sends the token in
  an `Authorization` header (not the URL, so it isn't in proxy logs).
- Prayer reminders are the first user: a push with a Prayed button
  (`prayer::reminder_notice`), counted as `prayers.push`.
- Still to do: Slipped / Snooze buttons, once habits (H3) and `+remind`
  (2.2) need them; a push count per person on the admin dashboard.

Done when:

- [ ] A test push arrives on both phones (Android and iPhone), away from
      home WiFi
- [ ] A button press reaches the server, and Prayed ticks the prayer off

### Stage 2.2: `+remind` (planned, expanded)

One-off reminders for the things families forget: library books due back,
the dentist, a parcel to return, the MOT.

Build:

- **Commands** (`+r` as a short form):
  - `+remind fri 10:00 dentist`: at that time, in the sender's timezone
  - `+remind 12-11 return library books`: a date with no time means the
    morning (09:00 local)
  - `+remind tmr bins`, `+remind 18:00 call the garage` (today)
  - **Early warnings, as a flag**, for things that need preparing:
    `+remind 15-11 14:30 dentist -b 1d,1h` reminds the day before and an
    hour before, as well as at the time. Library books:
    `+remind 12-11 library books -b 2d`.
  - **For someone else** in a group: `+remind @Khadija fri 10:00 dentist`
    sends it in the group, @mentioning her (her timezone).
  - `/remind` (`/r`): numbered list of upcoming reminders in this chat.
    Reply `remove 2` to cancel, or `snooze 2 1h` / `snooze 2 tmr`.
- **Dates** reuse the CLI MVP's `dates.rs` rules (`dd-mm`, `dd-mm-yyyy`,
  `today`, `tmr`), plus weekdays and `HH:MM`, all worked out in the
  person's own timezone and stored in UTC.
- **Todos with due dates** (Stage 1.7) get a reminder on the morning they're
  due, unless they're done.
- **One reminder engine**, extracted from the prayer one: a `reminders`
  table (chat, due time, source: prayer / todo / custom, source id, text,
  the WhatsApp message id once sent), **one sending loop** that claims
  before sending (so nothing goes twice) and catches up after downtime, and
  reactions routed back to the feature that owns the reminder (✅ on a todo
  reminder completes the todo).
- Reacting ✅ to a reminder marks it done; 😴 snoozes it an hour.
- **Ticking off by reaction** (moved from Stage 1.5): the "Added …" reply
  to `+shop` / `+todo` stores its message ID too, and ✅ or 👍 on it ticks
  that item off, through the same routing.

New concepts: refactoring a working feature into a shared one, flags with
values, a little date arithmetic.

Done when:

- [ ] `+remind` with a date, a time, both, and `-b` early warnings all fire
      at the right local time
- [ ] Reminders survive a restart, and ones missed while it was down are
      sent on start-up
- [ ] `/remind` lists them, and `remove` / `snooze` work
- [ ] A due todo gets a morning reminder, and none once it's done
- [ ] Prayer reminders run on the shared engine with no change in behaviour
- [ ] Reacting ✅ to an "Added …" message ticks that item off

### Stage 2.3: Prayer windows (mostly done early)

- Daily times from AlAdhan for each chat's own city, method and timezone,
  start and end of each prayer (Fajr ends at sunrise, Isha at Islamic
  midnight). ✅
- Still to do: if AlAdhan fails, use yesterday's times shifted one day,
  and log a warning (today it retries every minute until it answers).
- Done when:
  - [x] `/prayers` shows today's times (and the Hijri date)
  - [ ] They're checked correct across the BST/GMT clock changes (the code
        converts each time from AlAdhan's offset; a test for the change
        days themselves is still to write)
  - [ ] AlAdhan being down doesn't stop reminders

### Stage 2.4: 3-stage escalation (partly done early)

- Start reminder, and a warning shortly before the window ends that only
  @mentions the people who haven't checked in. ✅
- Check in with any reaction, or `+prayed`. ✅
- A cutoff that logs `Missed`. ✅ (see below)
- Still to do: a `PrayerStage` state machine
  (`NotStarted → Started → Warned → Done | Missed`) as a pure function,
  tested with a fake clock; the ntfy Done button.
- Done when: a full day runs correctly, tested with a fake clock.

#### Missed prayers and reports ✅ (0.14.0, done early)

- `prayer_log.status` (migration `0007`): `prayed`, `late` (made up) or
  `missed`. When a prayer's time ends, everyone its reminder was for who
  hasn't ticked it gets ❌ (`src/prayer_tracking.rs`). Only prayers a
  reminder actually went out for, so downtime never counts against anyone.
- Ticking a ❌ off later makes it 🕐 made up. `+prayed asr on time` /
  `late`, and `yesterday`, let people correct it.
- `/prayers week` and `/prayers month`: completion %, longest streak
  (on-time prayers in a row), misses per prayer, and an emoji heatmap.
- Reports, once Isha has ended: a 🎉 for praying all 5, plus the week's
  summary on Sundays and the month's on its last day. `/prayers reports
  here|dm|off` chooses where they go (default: the chat itself).
- Reports are generic (`src/reports.rs`, `report_settings`,
  `reports_sent`): a feature supplies the text, and the module handles
  when, where, and never twice. Habits (H6) should use it.
- Admin dashboard: each person's week and month, each group's members'
  week, and where reports go.
- 0.15.0: a month calendar on each person's page (any month, day numbers,
  each prayer on hover), and correcting any past day's five prayers
  (`PUT /api/users/{id}/prayers/{date}/{prayer}`).

Done when:

- [x] A prayer reminded but not ticked is ❌ after its time; a late tick is 🕐
- [x] Reports go once each, even across a restart (checked on a running
      server)
- [ ] A week of real family use: the Sunday summary reads right in WhatsApp

## Open questions

- Reminders (Stage 2.2): should a reminder for someone else in a group also
  be sent to their DM, so it's private?
