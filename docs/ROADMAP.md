# Family HQ: Roadmap

How Family HQ gets built: four phases, each split into stages. Every stage
ends with something that runs, and has a **Done when** checklist so it's
clear when to move on. Ticked boxes (`[x]`) are done and verified;
unticked ones aren't yet.

| Phase | What it delivers | Status |
|---|---|---|
| **1. Todo & Shop** | Shared shopping list and todos, driven from WhatsApp | Built and deployed (0.4.1); in family testing. Next: due dates, notes |
| **2. Reminders & Prayer Coach** | `+remind`, ntfy pushes, prayer windows, escalation, reflections | Prayer reminders done early; `+remind` planned |
| **3. Dashboard** | React web app served by the Rust binary | Later |
| **4. AI & Extras** | Weekly coaching, natural language, weather, calendars, Home Assistant | `/weather` and per-person timezones done early |
| **Admin dashboard** | The owner's view of users, settings and metrics, in its own container | In progress, at the owner's request: Done: A1 (0.8.0), A2 (0.8.1, 0.9.0), A3 (0.10.0), A4 (0.11.0) |

Rule: don't start a phase until the one before it has been **used by the
family for at least two weeks**. Real use finds the problems that matter.
The admin dashboard is outside this rule: it adds nothing for the family, and
its metrics help judge that testing.

---

## Decisions already made

- **The bot has its own WhatsApp number** (a dedicated SIM), connected via
  GOWA. Nobody's personal number is linked to it.
- **Rust** backend: Axum (web server) + Tokio (async runtime).
- **SQLite** via SQLx, a single file in a Docker volume.
- **Everything runs in Docker Compose** on the homelab: `family-hq` + `gowa`
  (+ `ntfy` from Phase 2). Released as `kaizenflow/family-hq` on Docker
  Hub and deployed from the `my-homeserver-stacks` repo with Dockhand.
- **All times are stored in UTC.** They're converted to the chat's own
  timezone (its settings; default `Europe/London`) only when deciding what
  "today" means or showing a time.
- **Settings belong to a chat.** A DM's settings are that person's own
  (timezone, city, prayer method and so on); a group's are shared. Unset
  values fall back to the `DEFAULT_*` values in `.env`.
- **Every list belongs to the chat it's used in.** Each group the bot is in
  has its own shopping list and todos, so several groups (e.g. different
  families) work side by side without mixing. A DM with the bot is that
  person's private list: `+shop` in a DM adds to your own list, not a
  group's.
- **Anyone who messages the bot is registered automatically**, named after
  the first word of their WhatsApp profile name (used for `@name`).
- **Commands start with `+` (add) or `/` (show or change).** Anything else
  is ordinary chat and ignored. Common commands have short forms (`+s`,
  `+t`, `/s`, `/w`, ...).

---

## Phase 1: Todo & Shop

**Goal:** each WhatsApp group the bot is in has a working shared shopping
list and todo list, and each person can keep private lists in a DM with the
bot.

### Commands (as built)

Every command works in a group or a DM and acts on **that chat's** lists.
`/help` in WhatsApp has a page per topic (`/help shop`, `/help todo`, ...).

| Message | Effect |
|---|---|
| `+shop milk, eggs, bread` (`+buy`, `+s`) | Adds each item to this chat's shopping list (skips ones already on it) |
| `/shop` (`/s`) | Numbered list: ⬜ still to buy, ✅ bought |
| reply `1 3` to a `/shop` list, or `+got 1 3` / `+got milk` (`+g`) | Ticks items off; in a group, "✅ @buyer bought …" |
| `/shop clear` / `/shop clear bought` | Empties the list / removes only ticked-off items |
| `+todo fix the tap` (`+t`) | Adds a todo, assigned to whoever sent it |
| `+todo @Khadija book dentist` | Adds a todo assigned to Khadija (a member of this group) |
| `/todo` (`/t`) | Numbered list of this chat's open todos |
| reply `done 2` / `remove 2` to a `/todo` list | Completes / deletes todo 2 |
| `/help` (`/h`) | Overview, plus a full page per topic |

The bot reacts 👍 to every message it understands as a command, so the
sender knows it was seen.

Numbers always refer to **the list message being replied to**, never
"whatever the list looks like now" (see `wa_message_context` below). That
avoids the number-shifting problem from the CLI MVP.

### Stage 1.1: Server skeleton ✅

Build:
- New Cargo project with `src/main.rs` and `src/lib.rs` (logic in the
  library, so tests can reach it).
- Axum server with `GET /health` returning `{"status":"ok"}`.
- Config from environment variables (port, database path).
- Dockerfile (reuse the two-stage one from `rust_todo_list`) and
  `compose.yaml`.

New concepts: `async fn`, `#[tokio::main]`, Axum routers and handlers.

Done when:
- [x] `curl localhost:3000/health` returns ok, both from `cargo run` and
      from `docker compose up`
- [x] Ctrl+C shuts the server down cleanly

### Stage 1.2: Database ✅ (HTTP API skipped)

Build:
- SQLx with migrations (`migrations/0001_init.sql`), run on startup with
  `sqlx::migrate!`.
- Tables: `users`, `chats`, `chat_members`, `todos`, `shopping_items`,
  plus the WhatsApp bookkeeping tables (see "Schema" below).
- An `AppState` holding the `SqlitePool`, shared with handlers.
- ~~JSON endpoints for testing by hand~~: not built. Testing went straight
  through signed webhooks instead, which exercise the real path.
- Repo modules: `repo/todos.rs`, `repo/shop.rs` (same idea as
  `task_repo.rs`).
- Error type with `thiserror` that Axum turns into an HTTP status.

New concepts: SQLx `query!` / `query_as!`, `cargo sqlx prepare` (offline
mode for Docker builds), `Arc`, Axum `State`, `Json`, serde, `impl IntoResponse`.

Done when:
- [ ] ~~You can add, list, complete and delete with `curl`~~ (no JSON API;
      done through WhatsApp instead)
- [x] Repo tests run against an in-memory database
- [x] The Docker build works without a live database (`.sqlx/` committed)

### Stage 1.3: Message parser ✅

Build:
- `parser.rs`: a **pure function** `parse(text: &str) -> Option<BotCommand>`.
  Returns `None` for ordinary chat (most group messages aren't commands).
- `BotCommand` enum: `AddShop(Vec<String>)`, `ListShop`, `ClearShop`,
  `AddTodo { title, assignee: Option<Assignee> }`, `ListTodos`, `Help`, …
- Reply parsing: `"1 3"` → `[1, 3]`, `"done 2"` → `Done(2)`.
- Lots of unit tests: extra spaces, capital letters, `+Shop`, empty
  `+shop`, `+todo @` with no name, commas vs `and`.

New concepts: nothing big. This is `cli.rs` again, with chat text as input.

Done when:
- [x] Every row of the command table above has a passing test
- [x] Normal chat ("see you at 6") parses to `None`

### Stage 1.4: Connect WhatsApp (GOWA) ✅

Build:
- Add the `gowa` service to `compose.yaml`; pair the bot's number by QR code.
- `POST /webhook/whatsapp`: receives GOWA events.
- Check the webhook signature using the shared secret from GOWA's config.
  Reject anything unsigned.
- Find the `chats` row for the message's chat ID (`@g.us` → group,
  `@s.whatsapp.net` → DM), creating it the first time someone uses
  the bot there. Every item saved gets that chat's `id`.
- Register new senders in `users` (matched by phone ID or LID), and ignore
  the bot's own messages.
- **Deduplicate:** store each WhatsApp message ID in `processed_messages`
  (primary key). If the insert fails because the ID is already there, it's a
  retry, so skip it.
- `whatsapp.rs`: a small client that sends a text reply via GOWA's REST API
  (`reqwest`).
- Wire it up: `+shop milk` in the group → saved → bot replies `🛒 Added milk`.

New concepts: webhooks, HMAC signature checks, `reqwest`, idempotency.

Done when:
- [x] `+shop` and `+todo` from a group and from a DM both work, each going
      on that chat's list
- [x] Two groups have separate lists
- [x] A first message from a new number registers them, once
- [x] Sending the same webhook body twice adds the item only once
- [x] A request with a bad signature gets `401`

### Stage 1.5: Lists and replying by number (mostly done)

Build:
- `/shop` and `/todo` send numbered lists.
- When sending a list, save its WhatsApp message ID and the
  number → row ID mapping in `wa_message_context`.
- When someone replies (quotes) to that message with `1 3` or `done 2`, look
  up the quoted message ID in `wa_message_context` to find the real rows.
- Replying to an old list still works, because the numbers are frozen
  per message.
- Not built yet: reaction ✅ or 👍 on a single "Added …" confirmation
  message also ticks that item off (the confirmation's message ID would be
  stored too). `+got milk` covers most of the need.

New concepts: reading quoted message IDs from the webhook payload, a
one-to-many mapping table.

Done when:
- [x] Two people can tick items off the same list without confusing each other
- [x] Replying `5` to a 3-item list gives a friendly error, not a crash
- [ ] Reacting ✅ to an "Added …" message ticks that item off

### Stage 1.6: Assignments and polish (partly done)

Build:
- `+todo @Khadija …` resolves the name against `users.display_name`
  (case-insensitive), among the members of this chat (`chat_members`).
  An unknown name gives an error reply listing valid names.
- `/todo` in the group shows who each todo is assigned to.
- `/help`, with a page per topic.
- Not built yet: logging with `tracing` (every webhook: who, which chat,
  which command, result). It uses `println!` for now.
- Not built yet: daily SQLite backup (`VACUUM INTO`) to a second volume.
  Kopia backs up `/opt/stacks/family-hq` meanwhile.

Done when:
- [x] `@name` assignment works, and an unknown name lists the valid ones
- [x] `/help` explains every command
- [ ] Logging with `tracing`
- [ ] Daily `VACUUM INTO` backup
- [ ] The family uses it for two weeks
- [ ] Every bug found in those two weeks has a test

### Built beyond the original plan

All of these are released and tested:

- **Todos like the shopping list:** `/todo` shows a progress bar
  (`■■■■□□□□□□ 4/10 done`), open ⬜ and done ✅ todos with who did each
  and when; `+done 2` / `+done gym` (`+d`) completes without replying;
  `/todo clear` and `/todo clear done`.
- **Prayer times by coordinates:** shared locations keep their exact point
  and typed cities are looked up with Open-Meteo (migration `0003`), so
  AlAdhan never has to geocode a name (its geocoder rate-limits and
  answered 503 for towns like Crawley).
- **Shopping:** `+got` to tick off by name or number without replying,
  ⬜/✅ in `/shop`, `/shop clear` vs `/shop clear bought`, duplicate items
  skipped, and the buyer @mentioned in groups.
- **Per-chat settings** (`/settings`, `/settings everyone`): timezone,
  city, prayer method, Asr school, reminder timing. Set a location by
  **sharing it in WhatsApp** (nearest town via OpenStreetMap Nominatim).
- **Short forms**: `+s +g +t +p`, `/s /t /p /w /m /h`.
- **Todo views**: `/todo` grouped by person, `/todo all`, `/todo <name>`,
  `/todo mine`, all with the same numbers.
- **Todo reminders**: `/todo reminders on`, per person per chat, four
  times a day on their own clock, only when they have open todos.
- **`/moon`** and the Islamic date and moon phase in `/prayers`.
- **Usage metrics**: every command, list reply, reminder reaction and
  unrecognised `+`/`/` word goes in `command_log` (names only, never
  message text); queries in `docs/METRICS.md`.
- **Deployment:** Docker Hub image, a stack in `my-homeserver-stacks`
  (Dockhand), and an entrypoint that fixes the data folder's ownership by
  itself.
- **Prayer times, reminders and `/weather`**, from Phases 2 and 4 (see the
  notes there).

### Stage 1.7: Todo due dates and priority flags (planned)

Bring the CLI MVP's flags (`cli.rs`'s `parse_flags`, `dates.rs`,
`priority.rs`) to WhatsApp todos.

Build:
- Flags after the title, as in the CLI:
  - `+todo renew passport -d 15-03` / `--due fri` / `--by tmr`
    (`dd-mm-yyyy`, `dd-mm`, `today`, `tmr`, weekday names)
  - `-p high` / `--priority h` (`high`, `medium`, `low`)
  - Both: `+t @Khadija book dentist -d mon -p high`
- Dates are worked out in the **sender's own timezone**, with `today`
  passed in (CLAUDE.md rule; the CLI's `parse_due_date` read the clock
  itself). A date in the past is an error, as in the CLI.
- `/todo` sorts as the CLI did: overdue first, then by due date, then
  priority. Each line shows `📅 Fri 15 Mar` and `🔴`/`🟠`/`🟢` for priority,
  and `⚠️ overdue` when it's late.
- Filters: `/todo overdue`, `/todo due`, `/todo high`, like the CLI's
  `-f` (`/todo mine` and `/todo <name>` already exist).
- Change after adding, by replying to a `/todo` list: `due 2 fri`,
  `due 2 none`, `priority 2 high`.
- Migration `0003`: `todos.due_date` (`date(due_date) IS due_date` CHECK,
  as in the CLI's `db.rs`) and `todos.priority` (`CHECK IN (...)`).

New concepts: porting your own CLI code, a second migration, sorting with
tuple keys (`sorted_tasks` again).

Done when:
- [ ] `+todo … -d fri -p high` saves both, and `/todo` shows them
- [ ] Overdue todos come first and are marked
- [ ] `due 2 …` / `priority 2 …` replies change them
- [ ] The parser has tests for every flag and date form, including bad
      ones (`-d`, `-p urgent`, a past date)

### Stage 1.8: Notes (planned)

Small things worth keeping and finding again: a driving licence number, the
Wi-Fi password for guests, a tyre size, the boiler model.

Build:
- `+note driving licence: SAMPL123456AB9CD` (`+n`): saves the note. Text
  before the first `:` is its title.
- `/notes` (`/n`): numbered list of titles. Reply with a number to see one,
  or `remove 2` to delete it.
- `/note licence`: shows notes whose title or text contains "licence".
- Notes belong to the chat like everything else: **a DM's notes are
  private**, a group's are shared with the group.
- `notes` table (migration `0004`): `chat_id`, `title`, `body`,
  `created_by`, `created_at`.

Privacy, decided before building:
- Notes are stored **unencrypted** in the SQLite file (and in Kopia's
  backups of it), and they stay in the WhatsApp chat history. That's fine
  for a licence number or a tyre size. **Passwords and bank details belong
  in Vaultwarden**, which the homelab already runs. `/help notes` should
  say so.
- Sensitive notes should live in a DM, not a family group.

Done when:
- [ ] `+note`, `/notes`, `/note <word>` and `remove` work in a DM and a group
- [ ] A DM's notes never show up in a group's `/notes`, or the other way round
- [ ] `/help notes` explains what not to store

### Stage 1.9: Clearing data nobody uses ✅ (0.12.0; on by default: `INACTIVE_DAYS=14`)

After `INACTIVE_DAYS` without using the bot, a person's or group's data is
deleted, after a warning `WARN_DAYS` (3) before. See `src/cleanup.rs`.

Build:
- Migration `0006`: `users.last_active_at`, `warned_at`, `cleared_at`;
  `chats.last_active_at`, `warned_at`, `left_at`.
- "Using the bot": a command (even one it didn't know), a list reply, a
  reaction to a reminder, or any message in a DM. Ordinary group chat isn't.
- A person: warned in their DM (if they have one), then their DM and
  everything in it, group memberships, todo reminders, prayer log and usage
  log are deleted. Their `users` row goes too, unless a group's list still
  names them (group lists are never touched); then it's deleted once nothing
  does.
- A group nobody uses the bot in: warned in the group, then the bot leaves
  it (GOWA `/group/leave`) and the group is deleted with everything in it.
- A group the bot was removed from: deleted once GOWA hasn't listed it for a
  day. An empty list from GOWA is never trusted.
- The rules are pure functions (`cleanup::due`, `clears_on`, `group_step`);
  clearing re-checks activity inside its transaction, so a message that
  arrives meanwhile keeps the data.
- Admin dashboard: when each person and group will be cleared, and "Clear
  their data now" / "Leave and delete now".

Done when:
- [x] Clearing deletes everything in the plan and leaves group lists intact
- [x] The `users` row goes once nothing names them
- [x] A message, list reply or reaction counts as activity; group chat doesn't
- [x] A reply to the warning keeps everything
- [x] With `INACTIVE_DAYS=0` nothing is ever deleted

### Schema

The real schema is the files in [`migrations/`](../migrations), with
comments on every table and constraint. Never edit one that has run; add a
new file. In short:

| Table | Holds |
|---|---|
| `users` | People who use the bot: `display_name` (case-insensitive, for `@name`), `wa_jid` (phone ID), `wa_lid` (LID) |
| `chats` | Each group or DM the bot works in: `wa_chat_id`, `kind` (`group`/`dm`), group `name`, `dm_user_id` |
| `chat_members` | Who is in which group, so `@name` only matches people in that group |
| `chat_settings` | Per-chat settings: timezone, city and its coordinates (`0003`), prayer method/school/reminder timing, reminders on/off |
| `shopping_items` | Items on a chat's list: `chat_id`, `name`, `added_by`, `bought_by` + `bought_at` |
| `todos` | Todos on a chat's list: `chat_id`, `title`, `created_by`, `assignee_id`, `status`, `completed_by` + `completed_at` |
| `prayer_reminders` | Each prayer reminder sent, per chat, day, prayer, kind (start/ending) and place (`0002`) |
| `prayer_log` | Who prayed what, per person per day |
| `processed_messages` | WhatsApp message IDs already handled, so webhook retries can't add things twice |
| `wa_message_context` | "Item 2 in the list the bot sent in message X, in chat Y" → the real row |

Every list row has a `chat_id`; that's what keeps groups apart and makes a
DM a private list. All tables are `STRICT` (SQLite rejects wrong types),
with `CHECK` constraints as a safety net behind Rust's own validation.

`wa_message_context.item_id` can't have a foreign key, because it points at
one of two tables. Instead, the code checks the row still exists when it's
used. Old rows in `wa_message_context` and `processed_messages` can be
deleted after 30 days.

### Layout

```
family-hq/
├── Cargo.toml
├── compose.yaml            # local dev stack (family-hq + gowa + echo)
├── Dockerfile              # two-stage build; image on Docker Hub
├── docker-entrypoint.sh    # fixes /data ownership, drops to PUID:PGID
├── deploy/                 # plain-compose deploy (the homelab uses Dockhand)
├── migrations/             # 0001 init, 0002 reminders per place, 0003 coordinates
├── .sqlx/                  # SQLx offline query data (committed)
├── docs/                   # ROADMAP.md, GOWA.md, DEPLOY.md
├── web/                    # the admin dashboard: React app, Dockerfile, Caddyfile
└── src/
    ├── main.rs             # starts Tokio, builds AppState, runs the server
    ├── lib.rs              # AppState, Apis, the router
    ├── config.rs           # env vars → Config
    ├── error.rs            # AppError (thiserror)
    ├── db.rs               # open the pool, run migrations
    ├── parser.rs           # text → BotCommand (pure, heavily tested)
    ├── handlers.rs         # BotCommand → database changes → reply
    ├── help.rs             # /help pages
    ├── person.rs           # Person and Assignee
    ├── settings.rs         # per-chat settings and defaults
    ├── prayer.rs           # prayers, windows, "what's due" (pure)
    ├── reminders.rs        # the prayer reminder loop
    ├── aladhan.rs          # prayer times API
    ├── geo.rs              # location → nearest town, town → coordinates
    ├── weather.rs          # Open-Meteo forecasts
    ├── whatsapp.rs         # GOWA REST client
    ├── routes/             # api.rs, health.rs, webhook.rs
    └── repo/               # chats, messages, prayers, settings, shop, todos, users
```

---

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

### Stage 2.5: Habits and reflections
- `habits` (BUILD / QUIT) and `daily_checkins` tables. Habit reminders use
  the Stage 2.2 engine.
- ❌ or 👎, or a missed cutoff → "What got in the way?" with options
  `1 Work  2 Sleep  3 Procrastination  4 Travel`. The reply is stored as an
  obstacle category plus an optional note.
- `/status`: streaks and the most common obstacle this week.
- Done when: a week of check-ins produces a sensible `/status`.

---

## Phase 3: Dashboard

- React + Vite + Tailwind + DaisyUI, built into `web/dist` and embedded in
  the Rust binary (`rust-embed` or similar).
- Read-only first: lists, notes, reminders, streaks, obstacle charts. Then
  editing.
- Login: a magic link sent over WhatsApp.
- Installable as a PWA (an app on the phone's home screen).

## Admin dashboard

An owner-only tool, separate from the family-facing dashboard in Phase 3: see
who uses the bot and what they have, read the metrics, and add or update an
individual user's settings.

Decisions:
- **Its own container** (`web/`): React, Vite, TypeScript, Tailwind and MUI
  icons, built into plain files that Caddy serves. Caddy passes `/api` to
  family-hq, so the browser sees one address and no CORS is needed. Port 3000
  and the webhook stay private.
- **Reached over the home network or Tailscale**, never port-forwarded.
- **A password, the same login as GOWA's** (`GOWA_BASIC_AUTH`), because
  being on the Wi-Fi shouldn't be enough to read everything. Sharing it means
  one login to set up rather than two; anyone who has it can already use
  GOWA as the bot, so a second password wouldn't protect much more. The
  browser asks for it (HTTP Basic auth), and Rust checks it on everything
  under `/api` (`src/routes/admin_auth.rs`).
- **DM lists show counts only**, because `/help` promises they're private. The
  API enforces it: a DM's list endpoints answer 404, so titles never reach the
  browser. Group lists show in full.
- **Exact coordinates are never sent to the browser**: a shared location can be
  someone's home. The town and timezone are enough.
- Settings changes don't message the user. Each one goes in the server log.

Not planned for now: editing lists, editing group settings, deleting users, an
audit history, several admins.

### Stage A1: Skeleton and release ✅ (first deploy to do)

Build:
- The `web/` shell (Users and Metrics pages are placeholders), `web/Dockerfile`
  and `web/Caddyfile`.
- `src/routes/api.rs`: `/api/health`, and a JSON 404 for anything else there.
- An `admin` service in both compose files, and the second image
  (`kaizenflow/family-hq-admin`) in the release workflow.

New concepts: nested routers, fallback routes.

Done when:
- [x] Refreshing a deep link works
- [x] `/api/nope` returns a JSON 404
- [x] The webhook can't be reached through the dashboard
- [ ] Deployed, and port 3000 still isn't reachable from another machine

### Stage A2: Users and what they have (read-only) ✅

Build:
- ✅ The password middleware (GOWA's login).
- ✅ Non-`GET` requests must be `application/json`. Browsers send Basic Auth
  by themselves, and this stops another website triggering changes.
- ✅ `AppError::NotFound` (404, JSON). A bad id in the path is already a 400
  from Axum's `Path` extractor, so no 400 variant was needed yet.
- ✅ Users page: name, last seen, uses in 30 days, groups, private chat. User
  page: their settings, marked as their own or the default, plus each chat's
  lists (group) or counts (DM).
- ✅ `GET /api/users`, `/api/users/{id}`, `/api/chats/{id}/todos`,
  `/api/chats/{id}/shop` (`src/routes/admin.rs`, queries in
  `src/repo/admin.rs`).
- ✅ Group names: `GET /api/users` saves them from GOWA's `/user/my/groups`
  (best effort, 5-second limit), into `chats.name`.
- `chat_members` never removes anyone, so a group someone left still shows.

New concepts: `axum::middleware`, `Serialize`, API types separate from database
types.

Done when:
- [x] A wrong or missing password gets 401 on every `/api` route
- [x] A DM's list answers 404, and no DM title or item is in any API response
- [x] No API response contains coordinates
- [x] Group lists match `/todo` and `/shop` (the same queries, in the same
      order)

### Stage A3: Metrics (read-only) ✅

Build:
- ✅ The queries from [METRICS.md](METRICS.md), in `src/repo/metrics.rs`;
  `GET /api/metrics?days=7` (no `days`: all time) in
  `src/routes/admin_metrics.rs`.
- ✅ For 7 days, 30 days or all time: uses per day, top commands, per person
  (including inactive people), busiest hours, unknown commands, failures,
  slowest commands.
- ✅ Plain Tailwind bars, no chart library, with a tooltip per bar and a
  table view.
- ✅ Days and hours are grouped in Rust (`metrics::per_day`, `by_hour`) on
  the default timezone's clock, from the UTC times in the log.

New concepts: query parameters, aggregate SQL with SQLx macros.

Done when:
- [x] Totals match the same SQL run directly
- [x] Busiest hours are correct in both BST and GMT (including the night the
      clocks go back)

### Stage A4: Edit a user's settings ✅

Build (all done):
- `src/settings_edit.rs`: the checks and saving for every change, used by
  both `/settings` and the dashboard; each words its own replies.
- `src/routes/admin_settings.rs`: `PATCH /api/users/{id}/settings` (all or
  nothing, in one transaction), `POST .../settings/reset`,
  `.../prayer-reminders`, `.../chats/{chat}/todo-reminders`, and
  `GET /api/prayer-methods`.
- `chats::find_or_create` falls back to a person's existing DM, so a row the
  dashboard created can't break their messages under another chat ID.

As planned:
- City and country, timezone, prayer method, Asr school, "ending soon" minutes,
  prayer reminders on/off, todo reminders on/off per chat, reset to defaults.
- `PATCH /api/users/{id}/settings`, and small `POST`s for reset and reminders.
- First move the validation out of `settings_command`, and make `save_city`
  independent of the WhatsApp context, so the bot and the API share one set of
  rules.
- Settings live on the DM chat row, which only exists once someone has messaged
  the bot privately. The API creates it using their `wa_jid`.

New concepts: partial updates with `Option`, sharing validation between two
callers, friendly API errors.

Done when:
- [x] A bad timezone or a 121-minute value gets a clear error and saves nothing
- [x] `/settings` in WhatsApp shows the change, and reminders follow it
- [x] A person's first real DM after their row was created uses that same row
      (`dm_user_id` is unique, so a mismatched ID would break all their DMs)

### Stage A5: Groups and per-chat metrics ✅ (0.13.0)

Build (all done):
- `src/routes/admin_groups.rs`: `GET /api/groups` (most recently active
  first) and `GET /api/groups/{id}` (settings, members, counts), plus
  `PATCH .../settings`, `POST .../settings/reset` and
  `.../prayer-reminders` for the group's own settings. A DM's ID is a 404
  on all of them.
- `admin_settings::apply_patch`: one set of rules for a person's and a
  group's settings.
- Web: a Groups page and a page per group (settings, lists, members linking
  to their pages, and the bot leaving it); group names on a user's page
  link there.

- Metrics per person and per group: `/api/metrics` takes `?user=` or
  `?chat=` (same `command_log`, no new tracking), and says which chats the
  bot was used in. The user and group pages show a "Usage" report.

Done when:
- [x] A group's settings can be changed and reset, and a refused change
      saves nothing
- [x] No member's number or anyone's coordinates are sent
- [x] One person's or one group's totals add up to theirs in the full report

## Phase 4: AI & Extras

In rough priority order. Each is optional and independent.

1. Weekly coaching summary from check-ins and obstacles (OpenRouter, small model).
2. Natural-language input ("remind me to call the garage tomorrow"), used
   only when the strict parser returns `None`.
3. 8:00 morning briefing: weather and today's todos and reminders.
   **`/weather` (now, tomorrow, week; Open-Meteo) is already built**; the
   briefing would reuse it.
4. Daily couple's conversation prompts.
5. Travel-aware timezones for prayer windows and reminders. **Mostly done**:
   each person has their own timezone and city, set by sharing their
   location. Still to do: noticing travel automatically.
6. Google Calendar sync and clash detection.
7. Home Assistant presence and ambient alerts.

---

## Open questions

- ~~Which GOWA version, and what does its webhook payload look like?~~
  Answered: GOWA v9.7.0 (pinned). Real payload shapes for text, replies,
  reactions, stickers and locations are in the webhook tests.
- Should `/shop clear bought` also run automatically, for example deleting
  bought items after 24 hours?
- Can personal todos be assigned to the other person from a DM, or is
  assignment group-only? (Today `@name` only finds members of the chat, so
  in a DM only yourself.)
- Backups: where should the second copy live (another disk, or cloud
  storage)? Kopia covers `/opt/stacks/family-hq` for now.
- Reminders (Stage 2.2): should a reminder for someone else in a group also
  be sent to their DM, so it's private?
