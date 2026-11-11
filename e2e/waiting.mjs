// "Waiting for Obscurify" never stays up forever while the server is reachable (customer report
// 2026-11-09: long sessions ended stuck on it). Each wait is driven into a failure that never
// clears and must end on its own. A lost connection (outage.mjs) is still waited out.
// With flaky-proxy.mjs running (see there): ACCOUNT=<funded number> node e2e/waiting.mjs
import { launch, ok, sleep, flip } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const script = (s) => fetch('http://127.0.0.1:8093/__script', { method: 'POST', body: JSON.stringify(s) });

await ctl('__set');
let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();

const say = async (text) => {
  await page.locator('.bar .icon').last().click();
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
};
// Seconds until the answer ends (or `limit`), and the longest the waiting note stayed up.
const untilIdle = async (limit) => {
  const t0 = Date.now();
  let since = 0, longest = 0;
  while (Date.now() - t0 < limit * 1000) {
    const [busy, waiting] = await page.evaluate(() => [!!document.querySelector('.send.stop'), !!document.querySelector('.waiting')]);
    if (waiting) since = since || Date.now(); else if (since) { longest = Math.max(longest, Date.now() - since); since = 0; }
    if (!busy) return { seconds: Math.round((Date.now() - t0) / 1000), longest: Math.round(longest / 1000), done: true };
    await sleep(1000);
  }
  return { seconds: limit, longest: Math.round(Math.max(longest, since ? Date.now() - since : 0) / 1000), done: false };
};
const last = () => page.locator('.msg:not(.user)').last().textContent();

// 1. Web search answers 503 every time: the tool call gives up, the run sees the error, the turn ends.
await ctl('__set?toolfail=-1');
await script([
  { code: 'try:\n    r = await tools.web_search("capital of France")\n    print("GOT", r)\nexcept Exception as e:\n    print("ERR", e)' },
  { text: 'The search failed, so I could not look it up.' },
]);
await say('Search the web for the capital of France.');
let r = await untilIdle(240);
ok(r.done && r.longest < 90, `a web search that keeps failing ends the turn (${JSON.stringify(r)})`);
ok(/kept failing|ERR/.test(await last()), 'the run saw the tool error');
ok(!(await page.locator('.waiting').count()), 'no waiting note is left');

// 2. The sandbox never loads: the turn stops with an error instead of waiting forever.
await ctl('__set?sandboxdown=1');
await launch(false).then((x) => { page = x.page; });
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await script([{ code: 'print(17 * 23)' }, { text: 'It is 391.' }]);
await say('Compute 17 * 23 with Python.');
r = await untilIdle(400);
ok(r.done, `a sandbox that never starts ends the turn (${JSON.stringify(r)})`);
ok(/sandbox would not start/i.test(await last()), 'it says the sandbox would not start');
await ctl('__set');

// 3. Plain chat (Tools off) where the server answers 502 every time: an error, not a wait forever.
await flip(page, '.tools-switch');
await script(Array.from({ length: 12 }, () => ({ status: 502 })));
await say('Hello');
r = await untilIdle(150);
ok(r.done && r.seconds >= 20, `a chat the server keeps failing ends with an error (${JSON.stringify(r)})`);
ok(/isn't answering|Obscurify/i.test(await last()), 'it says so');
await script([]);
await flip(page, '.tools-switch');

console.log('waiting: all passed');
process.exit(0);
