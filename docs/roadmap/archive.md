# Archive: finished stages

> **Goal:** keep the record of finished work out of the files read day to day.
> **Status:** Done. Read only when asked.
> **Last updated:** 2026-11-02

## Phase 1: finished stages

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

### Stage 1.5: Lists and replying by number ✅

Build:

- `/shop` and `/todo` send numbered lists.
- When sending a list, save its WhatsApp message ID and the
  number → row ID mapping in `wa_message_context`.
- When someone replies (quotes) to that message with `1 3` or `done 2`, look
  up the quoted message ID in `wa_message_context` to find the real rows.
- Replying to an old list still works, because the numbers are frozen
  per message.
- Reacting ✅ to an "Added …" message to tick that item off moved to
  Stage 2.2 (2026-11-03), where reactions are routed to whichever feature
  owns the message. `+got milk` covers the need meanwhile.

New concepts: reading quoted message IDs from the webhook payload, a
one-to-many mapping table.

Done when:

- [x] Two people can tick items off the same list without confusing each other
- [x] Replying `5` to a 3-item list gives a friendly error, not a crash

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

## Admin dashboard: finished stages

### Stage A1: Skeleton and release ✅

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
- [x] Deployed, and port 3000 still isn't reachable from another machine
      (checked 2026-11-03 on 0.15.0: the stack publishes no port for
      family-hq; from the LAN, 8090 is the dashboard, `/api` asks for the
      password, and 3003 is GOWA)

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

- ✅ The queries from [METRICS.md](../METRICS.md), in `src/repo/metrics.rs`;
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

## Answered open questions

- ~~Which GOWA version, and what does its webhook payload look like?~~
  Answered: GOWA v9.7.0 (pinned). Real payload shapes for text, replies,
  reactions, stickers and locations are in the webhook tests.
