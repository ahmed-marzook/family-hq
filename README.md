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
[docs/GOWA.md](docs/GOWA.md). To run it on a home server from the Docker Hub
image, see [docs/DEPLOY.md](docs/DEPLOY.md).

## Commands

Each group the bot is in has its own lists; a DM with the bot is your own
private list.

Short forms: `+s` = `+shop`, `+g` = `+got`, `+t` = `+todo`, `+d` = `+done`, `+p` = `+prayed`,
`/s` = `/shop`, `/t` = `/todo`, `/p` = `/prayers`, `/w` = `/weather`,
`/h` = `/help`.

| Message | Effect |
|---|---|
| `+shop milk, eggs` | Add to this chat's shopping list |
| `/shop` | Show the list; reply `1 3` to tick items off, `remove 2` to delete one |
| `+got milk, eggs` or `+got 1 3` | Tick items off without replying (numbers are from the latest `/shop` list) |
| `/shop clear` | Empty the whole list (`/shop clear bought` removes only ticked-off items) |
| `+todo fix the tap` | Add a todo, assigned to you |
| `+todo @Khadija book dentist` | Add a todo assigned to someone in the group |
| `/todo` | Progress bar, open (⬜) and done (✅, who and when) todos; reply `done 2` to complete one |
| `+done 2` or `+done gym` | Complete todos without replying (numbers from the latest `/todo`, or part of the title) |
| `/todo clear` / `/todo clear done` | Empty the list / remove only completed todos |
| `/prayers` | Today's prayer times, ✅ for the ones you've prayed |
| `/prayers on` / `off` | Reminders when each prayer starts and before it ends; react to one once you've prayed |
| `+prayed asr` or `+p 3` | Mark a prayer as prayed without reacting (by name, or 1 Fajr to 5 Isha) |
| `/settings` | This chat's city, timezone and prayer settings (in a DM: your own); `/settings city Leicester, GB` etc. |
| `/settings everyone` | Each group member's own settings |
| `/weather`, `/weather tomorrow`, `/weather week` | The forecast for your own town |
| `/help` | Overview; `/help shop`, `/help todo`, `/help prayers`, `/help settings`, `/help weather` for a full page |
