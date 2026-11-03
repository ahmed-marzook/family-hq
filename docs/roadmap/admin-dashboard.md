# Admin dashboard

> **Goal:** an owner-only tool to see who uses the bot and what they have, read the metrics, and change user and group settings.
> **Status:** A1–A5 done (latest 0.15.0). Nothing open. Finished stages are in archive.md.
> **Last updated:** 2026-11-03

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

