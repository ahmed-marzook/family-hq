# Todo & Shop (Phase 1)

> **Goal:** each WhatsApp group has a working shared shopping list and todo list, and each person can keep private lists in a DM.
> **Status:** Done and in daily use (1.1–1.6, archived). Planned: due dates (1.7).
> **Last updated:** 2026-11-03

## Phase 1: Todo & Shop

**Goal:** each WhatsApp group the bot is in has a working shared shopping
list and todo list, and each person can keep private lists in a DM with the
bot.

### Commands (as built)

Every command works in a group or a DM and acts on **that chat's** lists.
`/help` in WhatsApp has a page per topic (`/help shop`, `/help todo`, ...).

| Message                                                           | Effect                                                                 |
| ----------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `+shop milk, eggs, bread` (`+buy`, `+s`)                          | Adds each item to this chat's shopping list (skips ones already on it) |
| `/shop` (`/s`)                                                    | Numbered list: ⬜ still to buy, ✅ bought                              |
| reply `1 3` to a `/shop` list, or `+got 1 3` / `+got milk` (`+g`) | Ticks items off; in a group, "✅ @buyer bought …"                      |
| `/shop clear` / `/shop clear bought`                              | Empties the list / removes only ticked-off items                       |
| `+todo fix the tap` (`+t`)                                        | Adds a todo, assigned to whoever sent it                               |
| `+todo @Khadija book dentist`                                     | Adds a todo assigned to Khadija (a member of this group)               |
| `/todo` (`/t`)                                                    | Numbered list of this chat's open todos                                |
| reply `done 2` / `remove 2` to a `/todo` list                     | Completes / deletes todo 2                                             |
| `/help` (`/h`)                                                    | Overview, plus a full page per topic                                   |

The bot reacts 👍 to every message it understands as a command, so the
sender knows it was seen.

Numbers always refer to **the list message being replied to**, never
"whatever the list looks like now" (see `wa_message_context` below). That
avoids the number-shifting problem from the CLI MVP.

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
- A new migration: `todos.due_date` (`date(due_date) IS due_date` CHECK,
  as in the CLI's `db.rs`) and `todos.priority` (`CHECK IN (...)`).

New concepts: porting your own CLI code, a second migration, sorting with
tuple keys (`sorted_tasks` again).

Done when:

- [ ] `+todo … -d fri -p high` saves both, and `/todo` shows them
- [ ] Overdue todos come first and are marked
- [ ] `due 2 …` / `priority 2 …` replies change them
- [ ] The parser has tests for every flag and date form, including bad
      ones (`-d`, `-p urgent`, a past date)

## Open questions

- Should `/shop clear bought` also run automatically, for example deleting
  bought items after 24 hours?
- Can personal todos be assigned to the other person from a DM, or is
  assignment group-only? (Today `@name` only finds members of the chat, so
  in a DM only yourself.)
- Backups: which disk should the daily copies live on (`BACKUP_PATH`)?
  And Kopia should back up that folder rather than the live database.
- Notes (`+note`, `/notes`): dropped for now (2026-11-03). Bring back if
  the family asks for it during the two-week test; the old plan is in git
  history.
