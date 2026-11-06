# CLAUDE.md

Guidance for Claude when working in this repository.

## Project

Family HQ: a self-hosted family productivity bot used from WhatsApp.
Rust (Axum + Tokio) backend, SQLite via SQLx, WhatsApp via GOWA on the
bot's own number, ntfy push notifications from Phase 2.

The plan lives in `docs/ROADMAP.md`. Work happens one stage at a time, in
order. Check which stage is current before suggesting work, and don't pull
in features from later phases (ntfy, React, AI, and the rest of the
reminder/prayer work) while Phase 1 (todo and shop) is still in progress.
Exception, at the owner's request: basic prayer times, reminders and
tracking were brought forward and already exist (see the note at the top
of `docs/roadmap/reminders-prayer.md`). Habits and group goals (Stage 2.5,
H1–H6) were brought forward too, but are planned only and not started.
Points for todos (P1–P2, `docs/roadmap/points.md`) were brought forward as
well; P1 and P2 are built.
The admin dashboard (React, in `web/`) is also
at the owner's request, and has its own file in the roadmap. It isn't the
family-facing dashboard in Phase 3, which is still later.

## Roadmap

- Start from `docs/ROADMAP.md`. It's only an index: one line per area,
  with its status and a link.
- Open only the linked file in `docs/roadmap/` that's relevant to the
  current task.
- Never read `docs/roadmap/archive.md` unless asked. Never read
  `docs/roadmap/roadmap.original.md`; it's a backup of the file from
  before the split.
- Keeping it up to date: when a stage is finished (every **Done when** box
  ticked), move it word for word into `archive.md`. Update its area's
  status line in the index and the **Status** / **Last updated** header of
  the detail file. New work goes in its area's file; a new area gets its own
  file (roughly 100–300 lines, same header) and one line in the index.
- Keep the index under ~100 lines. Relative links in `docs/roadmap/` need
  `../` to reach `docs/` and `../../` to reach the repo root.

## The owner is learning Rust

This project is how the owner is learning Rust, so how you help matters as
much as what you build.

- **Reviewing code:** build it, run it, run `cargo clippy` and
  `cargo fmt --check`, then give a grade, what was done well, and
  numbered fixes, most important first, with short code snippets.
  Test real edge cases by running the program rather than only reading it.
- **Don't rewrite the owner's code unless asked.** Point out the problem and
  show the idiomatic pattern; the owner makes the change.
- **Explain new Rust concepts** (async, lifetimes, traits, `Arc`, SQLx
  macros) in plain words when they first come up.
- **Keep the owner's comments.** Comments that explain Rust itself are
  deliberate learning notes, not clutter. Fix only ones that are wrong or
  copy-pasted into the wrong place.

## Commands

```sh
cargo run                      # start the server
cargo test                     # all tests
cargo clippy --all-targets     # lints, should be warning-free
cargo fmt                      # format before every commit
cargo sqlx prepare -- --all-targets  # refresh .sqlx/ after changing a query, test queries too (commit it)
docker compose up --build      # run the full stack
npm run dev                    # in web/: the dashboard, with /api proxied to :3000
npm run build                  # in web/: type-check and build
```

## Conventions

- Logic lives in the library (`src/lib.rs` and modules); `main.rs` only
  wires things together and starts the server.
- `parser.rs` is a pure function: text in, `Option<BotCommand>` out. No
  database, no network. It should have the most tests of any module.
- All SQL lives in `src/repo/`. Use SQLx placeholders, never `format!` with
  user input.
- Schema changes are new files in `migrations/`. Never edit a migration that
  has already run.
- Use `CHECK` constraints in the schema as a safety net, and validate in Rust
  as well so users get friendly messages.
- Store all times in UTC. Convert to the chat's own timezone (from its
  settings, default `Europe/London`) only to decide what "today" means and
  to show times: `Settings::today(now)` / `Settings::clock(time)`. Pass
  `now`/`today` into functions instead of reading the clock inside them, so
  they can be tested.
- Errors: a `thiserror` `AppError` type, not `String`.
- Deduplicate WhatsApp webhooks by message ID (`processed_messages`).
- Register senders automatically on their first message (`repo::users::find_or_register`); ignore the bot's own messages (`is_from_me`).
- Write emoji as exact constants in code (e.g. `const DONE: &str = "✅";`).
- The admin API never sends a DM's list contents (counts only) or anyone's
  exact coordinates. Enforce this in the API, not just the UI.

## Don't

- Don't commit `.env`, `*.db` or the GOWA session data.
- Don't link anyone's personal WhatsApp number; the bot uses its own SIM.
- Don't add dependencies without saying why.
