# Reminders & Prayer Coach (Phase 2)

> **Goal:** `+remind`, ntfy pushes, prayer windows, escalation, reflections.
> **Status:** Prayer reminders and check-ins done early; still to do: ntfy (2.1), `+remind` and the shared reminder engine (2.2), the AlAdhan fallback and BST/GMT test (2.3), Missed tracking (2.4).
> **Last updated:** 2026-11-02

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
> yesterday's-times fallback, "missed" tracking, and ntfy.
>
> **Also brought forward, at the owner's request (2026-11-02):** habits and
> group goals, Stage 2.5 below, built as stages H1–H6.

### Stage 2.1: ntfy push notifications

- Add the `ntfy` container. Each user subscribes to their own topic.
- `notify.rs`: send a push with action buttons (Done / Slipped / Snooze).
- Buttons call back to `POST /api/actions/...` with a per-user token.
- Done when: a test push arrives on both phones and a button press reaches
  the server.

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
- Still to do: a cutoff that logs `Missed`; a `PrayerStage` state machine
  (`NotStarted → Started → Warned → Done | Missed`) as a pure function,
  tested with a fake clock; the ntfy Done button.
- Done when: a full day runs correctly, tested with a fake clock.

## Open questions

- Reminders (Stage 2.2): should a reminder for someone else in a group also
  be sent to their DM, so it's private?
