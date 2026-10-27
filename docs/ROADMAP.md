# Family HQ: Roadmap

How Family HQ gets built: four phases, each split into stages. Every stage
ends with something that runs, and has a **Done when** checklist so it's
clear when to move on.

| Phase | What it delivers | Status |
|---|---|---|
| **1. Todo & Shop** | Shared shopping list and todos, driven from WhatsApp | Next |
| **2. Reminders & Prayer Coach** | `+remind`, ntfy pushes, prayer windows, escalation, reflections | Later |
| **3. Dashboard** | React web app served by the Rust binary | Later |
| **4. AI & Extras** | Weekly coaching, natural language, weather, calendars, Home Assistant | Later |

Rule: don't start a phase until the one before it has been **used by the
family for at least two weeks**. Real use finds the problems that matter.

---

## Decisions already made

- **The bot has its own WhatsApp number** (a dedicated SIM), connected via
  GOWA. Nobody's personal number is linked to it.
- **Rust** backend: Axum (web server) + Tokio (async runtime).
- **SQLite** via SQLx, a single file in a Docker volume.
- **Everything runs in Docker Compose** on the homelab: `family-hq` + `gowa`
  (+ `ntfy` from Phase 2).
- **All times are stored in UTC.** They're converted to `Europe/London` only
  when deciding what "today" or "this morning" means.
- **Shopping is always family scope.** `+shop milk` sent in a DM still goes
  on the shared family list (handy when you're out). Todos follow the chat:
  group → family, DM → personal.
- **Only known people can use the bot.** Messages from WhatsApp IDs that
  aren't in the `users` table are ignored.

---

## Phase 1: Todo & Shop

**Goal:** the family WhatsApp group has a working shared shopping list and
todo list, and each person can keep private todos in a DM with the bot.

### Commands (Phase 1)

| Message | Where | Effect |
|---|---|---|
| `+shop milk, eggs, bread` (or `+buy`) | group or DM | Adds each item to the family shopping list |
| `/shop` | group or DM | Numbered list of items still to buy |
| reply `1 3` to a `/shop` list | group or DM | Ticks off items 1 and 3 |
| `/shop clear` | group or DM | Deletes ticked-off items |
| `+todo fix the tap` | group → family, DM → personal | Adds a todo |
| `+todo @Khadija book dentist` | group | Adds a family todo assigned to Khadija |
| `/todo` | group → family, DM → personal | Numbered list of open todos |
| reply `done 2` to a `/todo` list | same chat | Completes todo 2 |
| reply `remove 2` to a `/todo` list | same chat | Deletes todo 2 |
| `/help` | anywhere | Lists the commands |

Numbers always refer to **the list message being replied to**, never
"whatever the list looks like now" (see `wa_message_context` below). That
avoids the number-shifting problem from the CLI MVP.

### Stage 1.1: Server skeleton

Build:
- New Cargo project with `src/main.rs` and `src/lib.rs` (logic in the
  library, so tests can reach it).
- Axum server with `GET /health` returning `{"status":"ok"}`.
- Config from environment variables (port, database path).
- Dockerfile (reuse the two-stage one from `rust_todo_list`) and
  `compose.yaml`.

New concepts: `async fn`, `#[tokio::main]`, Axum routers and handlers.

Done when:
- [ ] `curl localhost:3000/health` returns ok, both from `cargo run` and
      from `docker compose up`
- [ ] Ctrl+C shuts the server down cleanly

### Stage 1.2: Database and HTTP API

Build:
- SQLx with migrations (`migrations/0001_init.sql`), run on startup with
  `sqlx::migrate!`.
- Tables: `users`, `todos`, `shopping_items` (draft schema below).
- An `AppState` holding the `SqlitePool`, shared with handlers.
- JSON endpoints for testing by hand: `GET/POST /api/shop`,
  `POST /api/shop/:id/bought`, `GET/POST /api/todos`,
  `POST /api/todos/:id/done`, `DELETE /api/todos/:id`.
- Repo modules: `repo/todos.rs`, `repo/shop.rs` (same idea as
  `task_repo.rs`).
- Error type with `thiserror` that Axum turns into an HTTP status.

New concepts: SQLx `query!` / `query_as!`, `cargo sqlx prepare` (offline
mode for Docker builds), `Arc`, Axum `State`, `Json`, serde, `impl IntoResponse`.

Done when:
- [ ] You can add, list, complete and delete with `curl`
- [ ] Repo tests run against an in-memory database
- [ ] The Docker build works without a live database (`.sqlx/` committed)

### Stage 1.3: Message parser (no WhatsApp yet)

Build:
- `parser.rs`: a **pure function** `parse(text: &str) -> Option<BotCommand>`.
  Returns `None` for ordinary chat (most group messages aren't commands).
- `BotCommand` enum: `AddShop(Vec<String>)`, `ListShop`, `ClearShop`,
  `AddTodo { title, assignee: Option<String> }`, `ListTodos`, `Help`, …
- Reply parsing: `"1 3"` → `[1, 3]`, `"done 2"` → `Done(2)`.
- Lots of unit tests: extra spaces, capital letters, `+Shop`, empty
  `+shop`, `+todo @` with no name, commas vs `and`.

New concepts: nothing big. This is `cli.rs` again, with chat text as input.

Done when:
- [ ] Every row of the command table above has a passing test
- [ ] Normal chat ("see you at 6") parses to `None`

### Stage 1.4: Connect WhatsApp (GOWA)

Build:
- Add the `gowa` service to `compose.yaml`; pair the bot's number by QR code.
- `POST /webhook/whatsapp`: receives GOWA events.
- Check the webhook signature using the shared secret from GOWA's config.
  Reject anything unsigned.
- `ChatScope` from the chat ID: ends in `@g.us` → `Family`, ends in
  `@s.whatsapp.net` → `Personal(user_id)`.
- Ignore senders not in `users`, and ignore the bot's own messages.
- **Deduplicate:** store each WhatsApp message ID in `processed_messages`
  (primary key). If the insert fails because the ID is already there, it's a
  retry, so skip it.
- `whatsapp.rs`: a small client that sends a text reply via GOWA's REST API
  (`reqwest`).
- Wire it up: `+shop milk` in the group → saved → bot replies `🛒 Added milk`.

New concepts: webhooks, HMAC signature checks, `reqwest`, idempotency.

Done when:
- [ ] `+shop` and `+todo` from the family group and from a DM both work
- [ ] A message from an unknown number does nothing
- [ ] Sending the same webhook body twice adds the item only once
- [ ] A request with a bad signature gets `401`

### Stage 1.5: Lists and replying by number

Build:
- `/shop` and `/todo` send numbered lists.
- When sending a list, save its WhatsApp message ID and the
  number → row ID mapping in `wa_message_context`.
- When someone replies (quotes) to that message with `1 3` or `done 2`, look
  up the quoted message ID in `wa_message_context` to find the real rows.
- Replying to an old list still works, because the numbers are frozen
  per message.
- Reaction ✅ or 👍 on a single "Added …" confirmation message also ticks
  that item off (the confirmation's message ID is stored too).

New concepts: reading quoted message IDs from the webhook payload, a
one-to-many mapping table.

Done when:
- [ ] Two people can tick items off the same list without confusing each other
- [ ] Replying `5` to a 3-item list gives a friendly error, not a crash

### Stage 1.6: Assignments and polish

Build:
- `+todo @Khadija …` resolves the name against `users.display_name`
  (case-insensitive). An unknown name gives an error reply listing valid names.
- `/todo` in the group shows who each todo is assigned to.
- `/help`.
- Logging with `tracing` (every webhook: who, which scope, which command,
  result).
- Daily SQLite backup (`VACUUM INTO`) to a second volume.

Done when:
- [ ] The family uses it for two weeks
- [ ] Every bug found in those two weeks has a test

### Phase 1 draft schema

```sql
CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    display_name  TEXT NOT NULL UNIQUE,            -- 'Bilal', 'Khadija'
    wa_jid        TEXT NOT NULL UNIQUE,            -- '4479...@s.whatsapp.net'
    created_at    TEXT NOT NULL
);

CREATE TABLE shopping_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
    added_by    INTEGER NOT NULL REFERENCES users(id),
    bought_by   INTEGER REFERENCES users(id),      -- NULL = still to buy
    bought_at   TEXT,
    created_at  TEXT NOT NULL,
    CHECK ((bought_by IS NULL) = (bought_at IS NULL))
);

CREATE TABLE todos (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
    scope         TEXT NOT NULL CHECK (scope IN ('family', 'personal')),
    owner_id      INTEGER REFERENCES users(id),    -- set for personal todos
    assignee_id   INTEGER REFERENCES users(id),    -- optional, family todos
    created_by    INTEGER NOT NULL REFERENCES users(id),
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'completed')),
    completed_at  TEXT,
    created_at    TEXT NOT NULL,
    -- personal todos must have an owner; family todos must not
    CHECK ((scope = 'personal') = (owner_id IS NOT NULL))
);

-- Every WhatsApp message already handled. Stops webhook retries
-- from adding things twice.
CREATE TABLE processed_messages (
    wa_message_id  TEXT PRIMARY KEY,
    received_at    TEXT NOT NULL
);

-- "Item 2 in the list I sent in message X" → the real row.
CREATE TABLE wa_message_context (
    wa_message_id  TEXT NOT NULL,       -- the bot's list/confirmation message
    position       INTEGER NOT NULL,    -- the number shown (1, 2, 3...)
    item_type      TEXT NOT NULL CHECK (item_type IN ('todo', 'shop')),
    item_id        INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (wa_message_id, position)
);
```

`item_id` can't have a foreign key, because it points at one of two tables.
Instead, the code checks the row still exists when it's used. Old rows in
`wa_message_context` and `processed_messages` can be deleted after 30 days.

### Planned layout

```
family-hq/
├── Cargo.toml
├── compose.yaml
├── Dockerfile
├── migrations/
│   └── 0001_init.sql
├── .sqlx/                 # SQLx offline query data (committed)
├── docs/
│   └── ROADMAP.md
└── src/
    ├── main.rs            # starts Tokio, builds AppState, runs the server
    ├── lib.rs
    ├── config.rs          # env vars → Config struct
    ├── error.rs           # AppError (thiserror) + IntoResponse
    ├── routes/
    │   ├── health.rs
    │   ├── api.rs         # JSON endpoints (Stage 1.2)
    │   └── webhook.rs     # GOWA webhook (Stage 1.4)
    ├── parser.rs          # text → BotCommand (pure, heavily tested)
    ├── scope.rs           # ChatScope from chat IDs
    ├── whatsapp.rs        # GOWA REST client
    ├── handlers.rs        # BotCommand → database changes → reply text
    └── repo/
        ├── users.rs
        ├── todos.rs
        ├── shop.rs
        └── messages.rs    # processed_messages + wa_message_context
```

---

## Phase 2: Reminders & Prayer Coach

Starts after Phase 1 has been in daily use for two weeks.

### Stage 2.1: ntfy push notifications
- Add the `ntfy` container. Each user subscribes to their own topic.
- `notify.rs`: send a push with action buttons (Done / Slipped / Snooze).
- Buttons call back to `POST /api/actions/...` with a per-user token.
- Done when: a test push arrives on both phones and a button press reaches
  the server.

### Stage 2.2: `+remind`
- `+remind 18:00 call the garage`, `+remind tmr 9am bins`. Reuse the date
  rules from the CLI MVP's `dates.rs`.
- `reminders` table (stored in UTC).
- Background task (`tokio::spawn` plus `tokio::time::interval`) sends due
  reminders via WhatsApp and ntfy, and marks them sent.
- If the server was down when a reminder was due, send it when the server
  starts again (catch-up).
- Done when: reminders fire on time, and survive a restart.

### Stage 2.3: Prayer windows
- Fetch daily times for Milton Keynes from AlAdhan just after **local**
  midnight. Store them in `prayer_windows` (date, prayer, start, end).
- Retry on failure; if it still fails, use yesterday's times shifted one
  day, and log a warning.
- Done when: `/prayers` shows today's times, and they're correct across the
  BST/GMT clock changes.

### Stage 2.4: 3-stage escalation
- A `PrayerStage` enum: `NotStarted → Started → Warned → Done | Missed`.
- Start: gentle group message + ntfy. Warning (30 minutes before the window
  ends): nudge only the people who haven't checked in. Cutoff: log `Missed`.
- Check in with ✅ / 👍, or the ntfy Done button.
- The state machine is a pure function, `next_stage(current, now, window)`,
  so it can be tested without waiting for real prayer times.
- Done when: a full day runs correctly, tested with a fake clock.

### Stage 2.5: Habits and reflections
- `habits` (BUILD / QUIT) and `daily_checkins` tables.
- ❌ or 👎, or a missed cutoff → "What got in the way?" with options
  `1 Work  2 Sleep  3 Procrastination  4 Travel`. The reply is stored as an
  obstacle category plus an optional note.
- `/status`: streaks and the most common obstacle this week.
- Done when: a week of check-ins produces a sensible `/status`.

---

## Phase 3: Dashboard

- React + Vite + Tailwind + DaisyUI, built into `web/dist` and embedded in
  the Rust binary (`rust-embed` or similar).
- Read-only first: lists, streaks, obstacle charts. Then editing.
- Login: a magic link sent over WhatsApp.
- Installable as a PWA (an app on the phone's home screen).

## Phase 4: AI & Extras

In rough priority order. Each is optional and independent.

1. Weekly coaching summary from check-ins and obstacles (OpenRouter, small model).
2. Natural-language input ("remind me to call the garage tomorrow"), used
   only when the strict parser returns `None`.
3. 8:00 morning briefing with weather (Open-Meteo) and today's todos.
4. Daily couple's conversation prompts.
5. Travel-aware timezones for prayer windows and reminders.
6. Google Calendar sync and clash detection.
7. Home Assistant presence and ambient alerts.

---

## Open questions

- Which GOWA version, and what exactly does its webhook payload look like for
  text, replies (quoted messages) and reactions? Save a few real payloads as
  test fixtures in Stage 1.4.
- Should `/shop clear` also run automatically, for example deleting bought
  items after 24 hours?
- Can personal todos be assigned to the other person from a DM, or is
  assignment group-only?
- Backups: where should the second copy live (another disk, or cloud storage)?
