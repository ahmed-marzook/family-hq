# Family HQ

A self-hosted family productivity bot. The family uses it from WhatsApp:
a shared shopping list and todos first, then reminders, prayer-time coaching
and habit tracking.

- **Backend:** Rust (Axum + Tokio)
- **Database:** SQLite (SQLx)
- **WhatsApp:** [GOWA](https://github.com/aldinokemal/go-whatsapp-web-multidevice), on the bot's own number
- **Push notifications:** ntfy (Phase 2)
- **Runs on:** Docker Compose on a homelab server

## Status

Phase 1 in progress. See [docs/ROADMAP.md](docs/ROADMAP.md) for the phases,
stages and schema. To run GOWA locally and experiment with WhatsApp, see
[docs/GOWA.md](docs/GOWA.md).

## Phase 1 commands (planned)

Each group the bot is in has its own lists; a DM with the bot is your own
private list.

| Message | Effect |
|---|---|
| `+shop milk, eggs` | Add to this chat's shopping list |
| `/shop` | Show the list; reply `1 3` to tick items off |
| `+todo fix the tap` | Add a todo, assigned to you |
| `+todo @Khadija book dentist` | Add a todo assigned to someone in the group |
| `/todo` | Show todos; reply `done 2` to complete one |
| `/help` | List commands |
