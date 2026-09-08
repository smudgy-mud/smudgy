// The calendar's storage and formatting, without the aliases. Everything here
// is a plain function over the database so it can be exercised outside Smudgy.
//
// Ported from the Mudlet package calendar-todo-list (Belgarath); see README.md
// for what is kept from the source and what is deliberately different.

import { eq, openDatabase, type Database, type DatabaseLocation, type Row } from "smudgy://official/mudlet-db";

/** `db:create("calendar", ...)` as the source declared it, options included. */
export const CALENDAR_SCHEMA = {
  todos: { text: "", status: "", timestamp: "", _index: [["text", "status", "timestamp"]] },
  events: { text: "", date: "", _index: [["text", "date"]] },
} as const;

export type Calendar = Database<typeof CALENDAR_SCHEMA>;
export type Todo = Row<typeof CALENDAR_SCHEMA.todos>;
export type CalendarEvent = Row<typeof CALENDAR_SCHEMA.events>;
export type TodoStatus = "incomplete" | "complete";

export function openCalendar(location: DatabaseLocation): Calendar {
  return openDatabase(location, CALENDAR_SCHEMA);
}

// ---------------------------------------------------------------------------
// Dates, as the source formatted them
// ---------------------------------------------------------------------------

const two = (value: number) => String(value).padStart(2, "0");

/** The source's `os.date("%d/%m/%y %I:%M:%S %p")`: local time, 12-hour clock. */
export function stamp(now: Date = new Date()): string {
  const hours12 = now.getHours() % 12 || 12;
  const meridiem = now.getHours() < 12 ? "AM" : "PM";
  return (
    `${two(now.getDate())}/${two(now.getMonth() + 1)}/${two(now.getFullYear() % 100)} ` +
    `${two(hours12)}:${two(now.getMinutes())}:${two(now.getSeconds())} ${meridiem}`
  );
}

export interface CalendarDate {
  readonly year: number;
  readonly month: number;
  readonly day: number;
}

/**
 * An event date as the source expected it, `dd/mm/yy`, with a two-digit year
 * read as 20yy. Anything else is not a date this calendar understands.
 */
export function parseEventDate(text: string): CalendarDate | undefined {
  const match = /^\s*(\d{1,2})\/(\d{1,2})\/(\d{2}|\d{4})\s*$/.exec(text);
  if (!match) return undefined;
  const [day, month, rawYear] = match.slice(1).map(Number);
  const year = rawYear < 100 ? 2000 + rawYear : rawYear;
  if (month < 1 || month > 12 || day < 1 || day > 31) return undefined;
  return { year, month, day };
}

export function today(now: Date = new Date()): CalendarDate {
  return { year: now.getFullYear(), month: now.getMonth() + 1, day: now.getDate() };
}

function sameDay(a: CalendarDate, b: CalendarDate): boolean {
  return a.year === b.year && a.month === b.month && a.day === b.day;
}

function isBefore(a: CalendarDate, b: CalendarDate): boolean {
  return a.year !== b.year ? a.year < b.year : a.month !== b.month ? a.month < b.month : a.day < b.day;
}

// ---------------------------------------------------------------------------
// To-dos
// ---------------------------------------------------------------------------

/** A row with the position the source's listings and `done <n>` refer to. */
export interface Numbered<T> {
  /** 1-based position in the whole sheet, complete rows included (as `ipairs` counted). */
  readonly position: number;
  readonly row: T;
}

function numbered<T>(rows: T[]): Numbered<T>[] {
  return rows.map((row, index) => ({ position: index + 1, row }));
}

export function addTodo(db: Calendar, text: string, now?: Date): Todo {
  const [rowId] = db.sheets.todos.add({ text, status: "incomplete", timestamp: stamp(now) });
  return db.sheets.todos.fetchOne(eq(db.sheets.todos.fields._row_id, rowId!))!;
}

/** The to-dos with `status`, numbered by their place in the full list. */
export function todosWithStatus(db: Calendar, status: TodoStatus): Numbered<Todo>[] {
  return numbered(db.sheets.todos.fetch()).filter(({ row }) => row.status === status);
}

/** `done <n>`: mark the n-th row of the full list complete; `undefined` when there is none. */
export function completeTodo(db: Calendar, position: number, now?: Date): Todo | undefined {
  const todo = db.sheets.todos.fetch()[position - 1];
  if (!todo) return undefined;
  todo.status = "complete";
  todo.timestamp = stamp(now);
  db.sheets.todos.update(todo);
  return todo;
}

/** `reset done`: remove every complete to-do. Returns how many went. */
export function clearCompleted(db: Calendar): number {
  return db.sheets.todos.delete(eq(db.sheets.todos.fields.status, "complete"));
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/** The source's `text:match("(.+) = (.+)")`: split at the last " = ". */
export function parseEventInput(input: string): { text: string; date: string } | undefined {
  const match = /^(.+) = (.+)$/.exec(input);
  if (!match) return undefined;
  return { text: match[1], date: match[2] };
}

export function addEvent(db: Calendar, text: string, date: string): CalendarEvent {
  const [rowId] = db.sheets.events.add({ text, date });
  return db.sheets.events.fetchOne(eq(db.sheets.events.fields._row_id, rowId!))!;
}

export interface ListedEvent extends Numbered<CalendarEvent> {
  /** Whether the event's date is today (`[TODAY]` in the listing). */
  readonly isToday: boolean;
}

export function listEvents(db: Calendar, now?: Date): ListedEvent[] {
  const current = today(now);
  return numbered(db.sheets.events.fetch()).map((entry) => {
    const date = entry.row.date === null ? undefined : parseEventDate(entry.row.date);
    return { ...entry, isToday: date !== undefined && sameDay(date, current) };
  });
}

/** `delevent <n>`: remove the n-th event; `undefined` when there is none. */
export function deleteEvent(db: Calendar, position: number): CalendarEvent | undefined {
  const event = db.sheets.events.fetch()[position - 1];
  if (!event) return undefined;
  db.sheets.events.delete(event);
  return event;
}

/**
 * `recycle events`: remove every event dated before today. Events whose date
 * does not parse are kept. Returns the removed events.
 */
export function recycleEvents(db: Calendar, now?: Date): CalendarEvent[] {
  const current = today(now);
  const stale = db.sheets.events.fetch().filter((event) => {
    const date = event.date === null ? undefined : parseEventDate(event.date);
    return date !== undefined && isBefore(date, current);
  });
  db.transaction(() => {
    for (const event of stale) db.sheets.events.delete(event);
  });
  return stale;
}

// ---------------------------------------------------------------------------
// The source's console tables
// ---------------------------------------------------------------------------

const RULE = "%".repeat(80);

/** The source's `%-10s%-40s%-28s%%%%` layout, each cell prefixed with `%% `. */
export function renderTable(header: readonly [string, string, string], rows: ReadonlyArray<readonly [string, string, string]>): string[] {
  const line = (cells: readonly [string, string, string]) =>
    `${`%% ${cells[0]}`.padEnd(10)}${`%% ${cells[1]}`.padEnd(40)}${`%% ${cells[2]}`.padEnd(28)}%%`;
  return [RULE, line(header), RULE, ...rows.map(line), RULE];
}
