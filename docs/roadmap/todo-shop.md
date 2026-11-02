# Todo & Shop (Phase 1)

> **Goal:** each WhatsApp group has a working shared shopping list and todo list, and each person can keep private lists in a DM.
> **Status:** Built and deployed; in family testing. Open: reaction tick-off (1.5), logging, backups and the two-week test (1.6). Planned: due dates (1.7), notes (1.8).
> **Last updated:** 2026-11-02

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

## Open questions

- Should `/shop clear bought` also run automatically, for example deleting
  bought items after 24 hours?
- Can personal todos be assigned to the other person from a DM, or is
  assignment group-only? (Today `@name` only finds members of the chat, so
  in a DM only yourself.)
- Backups: where should the second copy live (another disk, or cloud
  storage)? Kopia covers `/opt/stacks/family-hq` for now.
