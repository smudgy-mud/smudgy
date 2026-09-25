import { expect, test } from '@playwright/test';

test('a dropped initial ready message is recovered by the parent probe', async ({ page }) => {
  await page.route(/127\.0\.0\.1:8095\/runtime-broker\.js/, async (route) => {
    const response = await route.fetch();
    const source = await response.text();
    const ready = "window.parent.postMessage({ kind: 'smudgy-runtime-ready-v1' }, parentOrigin);";
    const initialReady = source.lastIndexOf(ready);
    expect(initialReady).toBeGreaterThan(-1);
    const body = `${source.slice(0, initialReady)}// The initial ready message was deliberately dropped by this test.${source.slice(initialReady + ready.length)}`;
    await route.fulfill({ response, body });
  });
  await page.goto('/');

  const connected = await page.evaluate(async () => {
    const preload = document.querySelector('link[rel="modulepreload"][href*="runtime-client.js"]');
    const { SessionProxy } = await import(preload.href);
    const proxy = new SessionProxy(() => {});
    try {
      await proxy.connect();
      proxy.terminate();
      return true;
    } catch {
      return false;
    }
  });
  expect(connected).toBe(true);
});

test('a temporary script-origin failure recovers without recreating the session', async ({ page }) => {
  let requests = 0;
  await page.route(/127\.0\.0\.1:8095\/runtime-frame\.html/, (route) => {
    requests += 1;
    if (requests === 1) return route.abort();
    return route.continue();
  });
  await page.goto('/');

  const connected = await page.evaluate(async () => {
    const preload = document.querySelector('link[rel="modulepreload"][href*="runtime-client.js"]');
    const { SessionProxy } = await import(preload.href);
    const proxy = new SessionProxy(() => {});
    try {
      await proxy.connect();
      proxy.terminate();
      return true;
    } catch {
      return false;
    }
  });
  expect(connected).toBe(true);
  expect(requests).toBeGreaterThan(1);
});

test('one broker outage reports one fatal event for all pending sessions', async ({ page }) => {
  await page.route(/127\.0\.0\.1:8095\/runtime-frame\.html/, (route) => route.abort());
  await page.goto('/');

  const kinds = await page.evaluate(async () => {
    const preload = document.querySelector('link[rel="modulepreload"][href*="runtime-client.js"]');
    const { SessionProxy } = await import(preload.href);
    const events = [];
    const proxies = Array.from({ length: 3 }, () => new SessionProxy((event) => {
      events.push(event.data.kind);
    }));
    proxies.forEach((proxy) => proxy.postMessage({ kind: 'start', packages: '[]' }));
    await new Promise((resolve) => setTimeout(resolve, 11000));
    proxies.forEach((proxy) => proxy.terminate());
    return events;
  });

  expect(kinds.filter((kind) => kind === 'runtime-fatal')).toHaveLength(1);
  expect(kinds.filter((kind) => kind === 'runtime-unavailable')).toHaveLength(2);
});
