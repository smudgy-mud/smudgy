# calendar-todo-list

A small to-do list and event calendar kept in a database, ported from the
Mudlet package of the same name by Belgarath (Mudlet forums, listed on
packages.mudlet.org; pinned in the smudgy-meta import research). It is the
first consumer of `smudgy://official/mudlet-db`, and it is small enough to read
in one sitting.

| Command | Does |
| --- | --- |
| `todo <text>` | Adds a to-do, stamped with the local time. |
| `todo` | Lists the to-dos still open, with their ID. |
| `done <id>` | Marks that to-do complete. IDs are the positions `todo` shows. |
| `done` | Lists the completed to-dos. |
| `reset done` | Deletes every completed to-do. |
| `event <what> = <dd/mm/yy>` | Adds a dated event. |
| `event` | Lists the events; today's are marked `[TODAY]`. |
| `delevent <id>` | Deletes that event. |
| `recycle events` | Deletes every event dated before today. |

The data lives in `Database_calendar.db` in the package's data directory, the
same file name and layout Mudlet used, so a copy of the original file drops in.

## What is kept from the source

- Both sheets and their compound indexes, exactly as `db:create` declared them.
- The `dd/mm/yy hh:mm:ss AM` timestamp text, and the `dd/mm/yy` event dates.
- IDs are positions in the full list, completed rows included, as the Lua
  `ipairs` loop counted them. Deleting or completing a row can therefore
  renumber the ones after it, as it did in Mudlet.
- The `%`-framed listing layout and the exact confirmation messages.

## Source defects, and what this port does about them

- `recycle events` indexed `updatedb[id]` with an `id` that was never set, so
  the first stale event raised a Lua error before anything was deleted. Here
  the stale events are deleted and each one is reported.
- The staleness test compared year, month and day separately
  (`year <= year and month <= month and day < day`), which misses an event
  from last month with a higher day number and treats December of last year
  as future. Here an event is stale when its date is before today.
- `event` without ` = ` in its text ran `db:add` with no columns, which DB.lua
  turned into a SQL error, and then errored again formatting the message.
  Here it prints how an event is written.
- An event whose date does not parse made `datetime:parse` raise while
  listing. Here it lists without the `[TODAY]` marker and is never recycled.

## Intentional differences

- A two-digit year is read as 20yy. Mudlet's `datetime:parse` was not
  consulted for its rule.
- Rows are written through `official/mudlet-db`, which throws on a failed
  write instead of returning nil, so a full disk or a locked file is reported
  rather than ignored.

## Running the tests

```
node --test test/
```

The tests run the package under host Node with `smudgy:core` stubbed and the
database dependency resolved from the sibling `packages/mudlet-db` folder.
