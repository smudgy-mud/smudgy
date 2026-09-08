# mudlet-db

Use your Mudlet database from a Smudgy script.

If your Mudlet script uses `db:create`, `db:add`, or `db:fetch`, this package gives you familiar operations in Smudgy. It can read the database files that Mudlet saves, so you can keep your existing records.

Your script still needs translating from Lua to TypeScript, the language Smudgy uses. This package provides the database functions. It does not translate or run your Lua script.

## Start with your saved data

You need Smudgy 0.5.5 or later. You do not need to install Node.js or SQLite separately.

The TypeScript examples below run in Smudgy. Save an example as a `.ts` file in your server's `modules` folder, usually under `Documents/smudgy/<server>/modules/`. Smudgy loads enabled modules when the session starts.

The `import` line makes this package's functions available to your script. If you are editing a shared package, also read [Package permissions](#package-permissions).

First, make a copy of your Mudlet database:

1. In Mudlet's Lua console, run `print(getMudletHomeDir())` to find your profile folder.
2. Close Mudlet before you copy the database file.
3. Find the `Database_*.db` file for your script in that folder.
4. Copy the file to a separate folder for your Smudgy data. Keep the original file in your Mudlet profile.

For example, a database called `combat log` uses the file `Database_combatlog.db`. Mudlet's naming rule removes digits and punctuation as well as spaces.

Use this script to check the copy. Replace the example path with the full path to your copied file. Forward slashes work in Windows paths too.

```ts
import { echo } from "smudgy:core";
import { openDatabase } from "smudgy://official/mudlet-db";

const mydb = openDatabase(
  { path: "C:/Users/YourName/Documents/MudData/Database_combatlog.db" },
  undefined,
  { readOnly: true, create: false },
);

for (const name of mydb.sheetNames) {
  const sheet = mydb.sheet(name);
  echo(`${name}: ${sheet.count()} saved rows`);
}

mydb.close();
```

A *sheet* is a named collection of rows, such as `enemies` or `kills`. Each row contains values such as a name, a city, or a date.

This example lists the sheets and their row counts without changing the file. `create: false` prevents a wrong path from creating a new, empty database. The `undefined` argument tells the package to read the sheet definitions from the file.

Check that the sheet names and counts match your saved data before you adapt the rest of your script.

## From a Mudlet script to a Smudgy script

Here is a small Mudlet example. It finds Ada's saved record and changes her city:

```lua
local mydb = db:create("people", {
  friends = { name = "", city = "" }
})

local rows = db:fetch(mydb.friends, db:eq(mydb.friends.name, "Ada"))
local ada = rows[1]

if ada then
  ada.city = "Boston"
  db:update(mydb.friends, ada)
end
```

The Smudgy version uses the same sheet and column names. Replace the path with the path to your copied `Database_people.db`:

```ts
import { openDatabase, eq } from "smudgy://official/mudlet-db";

const mydb = openDatabase(
  { path: "C:/Users/YourName/Documents/MudData/Database_people.db" },
  { friends: { name: "", city: "" } },
  { create: false },
);

const friends = mydb.sheets.friends;
const ada = friends.fetchOne(eq(friends.fields.name, "Ada"));

if (ada) {
  ada.city = "Boston";
  friends.update(ada);
}

mydb.close();
```

There are three changes to recognize:

- `mydb.sheets.friends` selects the sheet.
- `friends.fields.name` selects the column to search. `eq(..., "Ada")` means “equal to Ada.”
- `friends.fetchOne(...)` returns the first matching row. It returns `undefined` if no row matches, so the `if` check matters.

`friends.update(ada)` saves the changed record. Changing `ada.city` alone only changes the object in your script.

Use your original script's sheet definitions when you translate it. Include all existing columns and keep its `_unique` and `_violations` settings.

These examples close the database when their work finishes. If your aliases or triggers use the database later, keep it open while they need it.

## Create a new database

If you have no saved data to bring across, use a name and Smudgy's data directory:

```ts
import { getDataDir } from "smudgy:core";
import { openDatabase } from "smudgy://official/mudlet-db";

const mydb = openDatabase(
  { name: "people", directory: getDataDir() },
  { friends: { name: "", city: "" } },
);

mydb.sheets.friends.add({ name: "Ada", city: "Boston" });
mydb.close();
```

This creates `Database_people.db` if the file does not exist. Otherwise, it opens the existing file. Each run of this example adds another row.

`getDataDir()` returns the data folder for your script or package. It does not find or copy a Mudlet database for you.

## Common operations

The examples in this section assume that `friends` is an open sheet with `name` and `city` columns.

### Add a row

```ts
friends.add({ name: "Ada", city: "Boston" });
```

A column you omit receives its default value. Each saved row has an automatic `_row_id` number. Keep that number when you edit a fetched row.

### Find rows

```ts
import { eq, and, like } from "smudgy://official/mudlet-db";

const everyone = friends.fetch();
const bostonians = friends.fetch(eq(friends.fields.city, "Boston"));

const matchingFriends = friends.fetch(and(
  eq(friends.fields.city, "Boston"),
  like(friends.fields.name, "A%"),
));
```

`fetch()` returns a list of rows. The last query finds names that start with `A` in Boston. In a `like` pattern, `%` matches any number of characters and `_` matches one character.

To sort the result:

```ts
const alphabetical = friends.fetch(undefined, {
  orderBy: friends.fields.name,
});
```

Use `descending: true` to reverse the order. Use `limit` to restrict the number of results. `offset` skips rows and requires a `limit`.

### Change or delete a row

```ts
const ada = friends.fetchOne(eq(friends.fields.name, "Ada"));

if (ada) {
  ada.city = "London";
  friends.update(ada);
}
```

To delete that row instead, use `friends.delete(ada)` inside the `if` block. You can also pass a query to `delete`.

`friends.delete(true)` deletes every row in the sheet. Calling `delete()` without an argument is an error.

## Dates, empty values, and duplicate records

Column definitions work much like the table you pass to Mudlet's `db:create`. A column's default value determines its type.

| Column definition | Value in your script |
| --- | --- |
| `name: ""` | Text, or `null` |
| `level: 0` | A number, or `null` |
| `notes: null` | No fixed value type |
| `seen: timestamp()` | A JavaScript `Date`, or `null` |

Use `null` to store an empty database value. Omit a column, or use `undefined`, to leave it out of an add or update operation. For `add`, the omitted column receives its default. For `update`, its saved value stays unchanged.

For true/false values, choose numbers such as `1` and `0`, or strings such as `"true"` and `"false"`. The package does not accept JavaScript booleans.

For a date column, import `timestamp` and `CURRENT_TIMESTAMP`:

```ts
import { timestamp, CURRENT_TIMESTAMP } from "smudgy://official/mudlet-db";

const schema = {
  sightings: {
    name: "",
    seen: timestamp(CURRENT_TIMESTAMP),
  },
};
```

Pass `schema` as the second argument to `openDatabase`. New rows receive the current time when you omit `seen`. To supply a date yourself, use a `Date`, such as `new Date("2026-01-15T12:00:00Z")`.

Dates use Mudlet's storage format with one-second precision. Fetched dates are JavaScript `Date` objects, not Mudlet timestamp objects.

The familiar sheet options are also available:

| Option | Example | Meaning |
| --- | --- | --- |
| `_index` | `["city"]` | Add an index to help searches by city |
| `_index` | `[["name", "city"]]` | Add one index that covers both columns |
| `_unique` | `["name"]` | Prevent duplicate names |
| `_unique` | `[["name", "city"]]` | Prevent duplicate name-and-city pairs |
| `_violations` | `"IGNORE"` | Skip an added row when it conflicts with a unique value |

`_violations` defaults to `"FAIL"`. It also accepts `"IGNORE"`, `"REPLACE"`, `"ABORT"`, and `"ROLLBACK"`.

As in Mudlet, `"REPLACE"` deletes the conflicting row and inserts the new row. The new row gets a new `_row_id`. Columns omitted from the new row receive their defaults.

## Mudlet function reference

Here, `mydb` is the result of `openDatabase`, `sheet` is a sheet, and `field` comes from `sheet.fields`.

| In Mudlet | In Smudgy |
| --- | --- |
| `db:create(name, schema)` | `openDatabase({ name, directory }, schema)` |
| `db:get_database(name)` | Keep the `mydb` object returned by `openDatabase` |
| `mydb.friends` | `mydb.sheets.friends`, or `mydb.sheet("friends")` |
| `db:add(sheet, row, ...)` | `sheet.add(row, ...)` |
| `db:fetch(sheet, query)` | `sheet.fetch(query)` |
| The first fetched row | `sheet.fetchOne(query)` |
| `db:update(sheet, row)` | `sheet.update(row)` |
| `db:set(field, value, query)` | `sheet.set(field, value, query)` |
| `db:delete(sheet, what)` | `sheet.delete(what)` |
| `db:merge_unique(sheet, rows)` | `sheet.mergeUnique(rows)` |
| `db:fetch_sql(sheet, sql)` | `sheet.fetchSql(sql, params)` |
| `db:aggregate(field, fn, query, distinct)` | `sheet.aggregate(field, fn, query, distinct)` |
| `mydb:_drop(name)` | `mydb.dropSheet(name)` |
| `mydb:_begin()`, `:_commit()`, `:_rollback()` | `mydb.begin()`, `.commit()`, `.rollback()` |
| `mydb:_end()` | Commit or roll back explicitly |
| `db:close()` | `mydb.close()` for each database you opened |

`add` returns a list of new row IDs. An ignored row has `null` in that list. `set` and `delete` return the number of affected rows.

`mergeUnique` needs exactly one unique key with one column. Declare that key in the schema you pass to `openDatabase`. Each input row must contain its key value.

`sheet.count(query)` counts matching rows. `aggregate` accepts `COUNT`, `AVG`, `MAX`, `MIN`, and `TOTAL`. Its `query` and `distinct` arguments are optional.

### Query functions

Import the functions you use from `smudgy://official/mudlet-db`.

| In Mudlet | In Smudgy |
| --- | --- |
| `db:eq`, `db:not_eq` | `eq`, `notEq` |
| `db:lt`, `db:lte`, `db:gt`, `db:gte` | `lt`, `lte`, `gt`, `gte` |
| `db:is_nil`, `db:is_not_nil` | `isNull`, `isNotNull` |
| `db:like`, `db:not_like` | `like`, `notLike` |
| `db:between`, `db:not_between` | `between`, `notBetween` |
| `db:in_`, `db:not_in` | `isIn`, `notIn` |
| `db:AND`, `db:OR` | `and`, `or` |
| `db:exp` | `exp` |
| A table of query expressions | An array of expressions, combined with AND |
| `db:query_by_example` | `sheet.where`, for exact values only |

For example, `friends.where({ city: "Boston" })` finds an exact city value. It does not interpret Mudlet's special forms such as `>10`, `a||b`, or `x::y`. Use the query functions for those searches.

`eq` and `notEq` accept a third argument, `true`, for a comparison that ignores letter case.

For custom SQL, pass values separately: `exp("level > ?", [10])`. `fetchSql` also accepts a parameter list. Prefer the query functions for ordinary searches.

## Package permissions

Local modules can access your files. A shared package that runs in Smudgy's sandbox needs permission to access its data folder.

Add these entries to that package's `smudgy.package.json`. Keep its other entries:

```json
{
  "dependencies": ["smudgy://official/mudlet-db@^0.1"],
  "permissions": {
    "read": ["$DATA"],
    "write": ["$DATA"]
  }
}
```

`$DATA` means that package's data folder, which `getDataDir()` returns. This permission does not include your Mudlet profile folder or an arbitrary copied file's folder. The copied database must be in an allowed folder before the package can use it.

## If something goes wrong

Database operations throw an error when they fail. They do not return Mudlet's `nil` plus an error message.

| Error code | What to check |
| --- | --- |
| `argument` | Check the file path and the arguments to the function |
| `permission` | Check the package's file permissions and the database location |
| `schema` | Check sheet names, column names, and sheet options |
| `schema-mismatch` | Compare your sheet definitions with the existing database |
| `value` | Check the value you are trying to store or compare |
| `stored-value` | The file contains a value the package cannot read, such as an invalid timestamp |
| `closed` | The script used the database after calling `close()` |
| `transaction` | Check the calls that start or end a transaction |
| `unsupported` | Read the message for the operation or change that is unavailable |
| `sqlite` | Read the message for a database error, such as a duplicate unique value or a locked file |

To show a database error in Smudgy:

```ts
import { echo } from "smudgy:core";
import { MudletDbError } from "smudgy://official/mudlet-db";

try {
  friends.add({ name: "Ada", city: "Boston" });
} catch (error) {
  if (error instanceof MudletDbError) {
    echo(`Database error: ${error.message}`);
  } else {
    throw error;
  }
}
```

## Compatibility and larger scripts

The package uses `node:sqlite`, which Smudgy includes. It follows Mudlet's database naming and storage conventions.

Opening with a schema can create missing sheets, columns, and indexes. It preserves extra columns and indexes. `mydb.report` lists changes and differences. The package does not rebuild existing tables to change their unique constraints.

Fetched rows include preserved columns, which you can edit and save. Columns omitted from your schema keep their raw database value types.

`inspectDatabase(location)` reads the file's table definitions without changing them. `copyDatabase(source, destination)` creates a database snapshot, including committed data from an active write-ahead log. It refuses an existing destination by default.

Run `copyDatabase` from a local module or trusted package. Smudgy's sandbox blocks the SQLite operation it needs. A sandboxed package can open the completed copy with the appropriate file permissions.

`openDatabase` also accepts `timeout` in its options. This sets the wait for a database lock in milliseconds. The default is `5000`.

All operations run on the script thread. For many writes, use a transaction to save them together:

```ts
mydb.transaction(() => {
  friends.add({ name: "Ada", city: "Boston" });
  friends.add({ name: "Ben", city: "London" });
});
```

The callback must finish its work synchronously. Do not make it `async`. Successful completion commits the changes. An exception rolls them back. Nested calls use savepoints.

The `"ROLLBACK"` conflict policy ends the whole transaction, including any outer transaction. If you catch that error inside a callback, leave the callback before you continue using the database. Start a new transaction if you need one.

Foreign-key enforcement is off when the database opens. SQLite extensions and use from Smudgy workers are outside this package's supported interface.

The documented API is the supported contract for the `0.x` line. A breaking API change raises the major version. Exported schema helpers for conversion tools are not part of that contract.

### Performance

Although smudgy's mudlet-db library is just a bolt-on package, it's quite performant. On the same computer, with the same 10,000-row sheet, Smudgy needed about 40% of Mudlet's time to fetch rows and 60% to 90% of its time to write them.

| Operation | Mudlet | Smudgy |
| --- | ---: | ---: |
| Add one row | 2.8 ms | 1.7 ms |
| Add 9,000 rows in one transaction | 81 ms | 61 ms |
| Fetch 20 rows by an indexed column | 0.18 ms | 0.07 ms |
| Fetch all 10,000 rows | 70 ms | 27 ms |
| Search with `like` | 15 ms | 6.5 ms |
| Count rows | 0.32 ms | 0.21 ms |

Times are medians of three runs on one Windows workstation with Mudlet 5.0.1. Fetches are faster mostly because of inherent efficiencies in the JavaScript SQLite library smudgy uses. Writes are faster because the package prepares each statement once and binds values to it.

Adding rows one at a time costs about 1.7 ms each because each row is saved to disk separately. A transaction saves a group of rows together, which is much faster for bulk work.

### Tests

Package contributors can run the tests with Node.js:

```sh
node --test "test/*.test.ts"
```

The tests cover Mudlet database operations, a translated ShipDB storage example, and files created with Mudlet's SQL statements.
