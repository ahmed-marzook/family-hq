# Todo & Shop (Phase 1)

> **Goal:** each WhatsApp group has a working shared shopping list and todo list, and each person can keep private lists in a DM.
> **Status:** Done and in daily use (1.1–1.7 and 1.9, archived). Nothing open; ideas under Open questions.
> **Last updated:** 2026-11-06

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
| `+todo bins, hoover -d fri -p high`                               | Adds todos with a due date and priority (for all of them)              |
| reply `skip 2`, or `+skip 2` / `+skip gym`                        | Closes a todo as "won't be done", kept on record as ⏭️ skipped         |
| reply `due 2 fri` / `due 2 none` / `priority 2 high`              | Changes an open todo's due date or priority                            |
| `/todo overdue` / `/todo due` / `/todo high`                      | Only the open todos that are late / have a date / are high priority    |
| `/help` (`/h`)                                                    | Overview, plus a full page per topic                                   |

The bot reacts 👍 to every message it understands as a command, so the
sender knows it was seen.

Numbers always refer to **the list message being replied to**, never
"whatever the list looks like now" (see `wa_message_context` below). That
avoids the number-shifting problem from the CLI MVP.

## Open questions

- Should `/shop clear bought` also run automatically, for example deleting
  bought items after 24 hours?
- Can personal todos be assigned to the other person from a DM, or is
  assignment group-only? (Today `@name` only finds members of the chat, so
  in a DM only yourself.)
- Backups: which disk should the daily copies live on (`BACKUP_PATH`)?
  And Kopia should back up that folder rather than the live database.
- Should todo reminders show due dates and "⚠️ overdue"? They're already
  sorted overdue first (they use `todos::list_all`), but the lines don't
  show dates yet.
- Notes (`+note`, `/notes`): dropped for now (2026-11-03). Bring back if
  the family asks for it during the two-week test; the old plan is in git
  history.
