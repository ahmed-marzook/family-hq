# Overview

> **Goal:** the shape of the whole plan, the decisions behind it, and the schema and layout it builds on.
> **Status:** Reference; current.
> **Last updated:** 2026-11-02

# Family HQ: Roadmap

How Family HQ gets built: four phases, each split into stages. Every stage
ends with something that runs, and has a **Done when** checklist so it's
clear when to move on. Ticked boxes (`[x]`) are done and verified;
unticked ones aren't yet.

| Phase                           | What it delivers                                                      | Status                                                                                             |
| ------------------------------- | --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| **1. Todo & Shop**              | Shared shopping list and todos, driven from WhatsApp                  | Built and deployed (0.4.1); in family testing. Next: due dates, notes                              |
| **2. Reminders & Prayer Coach** | `+remind`, ntfy pushes, prayer windows, escalation, reflections       | Prayer reminders done early; habits brought forward (Stage 2.5, H1–H6); `+remind` planned          |
| **3. Dashboard**                | React web app served by the Rust binary                               | Later                                                                                              |
| **4. AI & Extras**              | Weekly coaching, natural language, weather, calendars, Home Assistant | `/weather` and per-person timezones done early                                                     |
| **Admin dashboard**             | The owner's view of users, settings and metrics, in its own container | In progress, at the owner's request: Done: A1 (0.8.0), A2 (0.8.1, 0.9.0), A3 (0.10.0), A4 (0.11.0) |

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

### Schema

The real schema is the files in [`migrations/`](../../migrations), with
comments on every table and constraint. Never edit one that has run; add a
new file. In short:

| Table                | Holds                                                                                                                  |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `users`              | People who use the bot: `display_name` (case-insensitive, for `@name`), `wa_jid` (phone ID), `wa_lid` (LID)            |
| `chats`              | Each group or DM the bot works in: `wa_chat_id`, `kind` (`group`/`dm`), group `name`, `dm_user_id`                     |
| `chat_members`       | Who is in which group, so `@name` only matches people in that group                                                    |
| `chat_settings`      | Per-chat settings: timezone, city and its coordinates (`0003`), prayer method/school/reminder timing, reminders on/off |
| `shopping_items`     | Items on a chat's list: `chat_id`, `name`, `added_by`, `bought_by` + `bought_at`                                       |
| `todos`              | Todos on a chat's list: `chat_id`, `title`, `created_by`, `assignee_id`, `status`, `completed_by` + `completed_at`     |
| `prayer_reminders`   | Each prayer reminder sent, per chat, day, prayer, kind (start/ending) and place (`0002`)                               |
| `prayer_log`         | Who prayed what, per person per day                                                                                    |
| `processed_messages` | WhatsApp message IDs already handled, so webhook retries can't add things twice                                        |
| `wa_message_context` | "Item 2 in the list the bot sent in message X, in chat Y" → the real row                                               |

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
├── docs/                   # ROADMAP.md (index) + roadmap/, GOWA.md, DEPLOY.md
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
