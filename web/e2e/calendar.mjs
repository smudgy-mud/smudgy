import { test as base } from '@playwright/test';

// Holiday costumes change the empty-state height. Offset Date for calendar
// tests while preserving native timers, performance.now and animation frames.
export function installCalendarDate(isoDate) {
  const NativeDate = globalThis.Date;
  const offset = NativeDate.parse(isoDate) - NativeDate.now();
  const now = () => NativeDate.now() + offset;
  globalThis.Date = new Proxy(NativeDate, {
    apply: () => new NativeDate(now()).toString(),
    construct: (target, args, newTarget) =>
      Reflect.construct(target, args.length ? args : [now()], newTarget),
    get: (target, property, receiver) =>
      property === 'now' ? now : Reflect.get(target, property, receiver),
  });
}

export const test = base.extend({
  calendarDate: ['2026-07-15T12:00:00Z', { option: true }],
  page: async ({ page, calendarDate }, use) => {
    await page.addInitScript(installCalendarDate, calendarDate);
    await use(page);
  },
});
