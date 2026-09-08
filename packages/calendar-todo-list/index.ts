// The calendar's aliases: `todo`, `done`, `reset done`, `event`, `delevent`
// and `recycle events`, printing what the Mudlet original printed.

import { createAlias, echo, getDataDir } from "smudgy:core";
import {
  addEvent,
  addTodo,
  clearCompleted,
  completeTodo,
  deleteEvent,
  listEvents,
  openCalendar,
  parseEventInput,
  recycleEvents,
  renderTable,
  todosWithStatus,
  type TodoStatus,
} from "./calendar.ts";

/** Where the Mudlet script kept it: `Database_calendar.db` in the profile directory. */
const calendar = openCalendar({ name: "calendar", directory: getDataDir() });

function printTodos(status: TodoStatus): void {
  const heading = status === "incomplete" ? "Date (since)" : "Date (done)";
  const rows = todosWithStatus(calendar, status).map(
    ({ position, row }) => [String(position), row.text ?? "", row.timestamp ?? ""] as const,
  );
  for (const line of renderTable(["ID", "Todo", heading], rows)) echo(line);
}

createAlias(/^todo(?: (?<text>.+))?$/, ({ text }) => {
  if (text) {
    const todo = addTodo(calendar, text);
    echo(`To-do item '${todo.text}' added to the database.`);
  } else {
    printTodos("incomplete");
  }
}, { name: "add todo" });

createAlias(/^done(?: (?<id>\d+))?$/, ({ id }) => {
  if (id) {
    const todo = completeTodo(calendar, Number(id));
    if (!todo) {
      echo(`Could not find entry #${id} in database.`);
      return;
    }
    echo(`To-do item '${todo.text}' marked as done.`);
  } else {
    printTodos("complete");
  }
}, { name: "done todo" });

createAlias(/^reset done$/, () => {
  clearCompleted(calendar);
  echo("Cleared all todos marked as complete.");
}, { name: "reset done" });

createAlias(/^event(?: (?<input>.+))?$/, ({ input }) => {
  if (input) {
    const parsed = parseEventInput(input);
    if (!parsed) {
      echo("An event is written as: event <what> = <dd/mm/yy>");
      return;
    }
    const event = addEvent(calendar, parsed.text, parsed.date);
    echo(`Event '${event.text}' at '${event.date}' added to the database.`);
  } else {
    const rows = listEvents(calendar).map(
      ({ position, row, isToday }) => [String(position), row.text ?? "", `${row.date ?? ""}${isToday ? " [TODAY]" : ""}`] as const,
    );
    for (const line of renderTable(["ID", "Event", "Date"], rows)) echo(line);
  }
}, { name: "add event" });

createAlias(/^delevent (?<id>\d+)$/, ({ id }) => {
  const event = deleteEvent(calendar, Number(id));
  if (!event) {
    echo(`Could not find entry #${id} in database.`);
    return;
  }
  echo(`Event '${event.text}' at '${event.date}' deleted from database.`);
}, { name: "delete event" });

createAlias(/^recycle events$/, () => {
  for (const event of recycleEvents(calendar)) {
    echo(`Event '${event.text}' at '${event.date}' recycled.`);
  }
}, { name: "recycle old events" });
