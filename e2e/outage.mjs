// A lost connection doesn't fail anything: requests wait and deliver when it's back. With
// flaky-proxy.mjs running (see there): ACCOUNT=<funded number> node e2e/outage.mjs
import { launch, ok, sleep } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const tools = async () => JSON.parse(await ctl('__tools'));
const SEARCH = 'Use run_code with Python: r = await tools.web_search("capital of France"); print("GOT", r["answer"][:80]). Then say what it printed. If the tool raises an error, print the error.';
const COMPUTE = 'Use run_code with Python: print("value", 1234*5678). Then tell me the printed value.';

let { page } = await launch(true);
await ctl('__set');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const say = async (text) => {
  await page.locator('.bar .icon').last().click();
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
};
const text = () => page.locator('.msg.assistant').last().textContent();

// 1. The connection goes while a tool call is out: it waits, then delivers.
await ctl('__set?tooldown=25000');
await say(SEARCH);
await page.waitForSelector('.waiting', { timeout: 120000 });
ok(true, 'it says it is waiting for the connection');
await idle();
let t = await text();
ok(!/ToolError|Can't reach/.test(t) && /paris/i.test(t), 'the tool call delivered after the outage: ' + t.slice(-80).replace(/\s+/g, ' '));
ok(await page.locator('.waiting').count() === 0 && await page.locator('.interrupted').count() === 0, 'and nothing is left waiting or stopped');

// 2. The connection goes between steps, with a finished run in hand.
await ctl('__set?chatdown=25000');
await say(COMPUTE);
await page.waitForSelector('.waiting', { timeout: 120000 });
await idle();
t = await text();
ok(t.replace(/,/g, '').includes('7006652') && await page.locator('.run').count() === 1 && await page.locator('.interrupted').count() === 0, 'the answer finished after the outage, with one run');

// 3. Stop still works while waiting.
await ctl('__set?tooldown=60000');
await say(SEARCH);
await page.waitForSelector('.waiting', { timeout: 120000 });
await page.locator('.send.stop').click();
await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 30000 });
ok(await page.locator('.waiting').count() === 0, 'Stop ends the wait');
await ctl('__set');

// 4. A paid call whose answer is lost: asked again with the same id, charged once.
if (process.env.PAID) {
  const balance = async () => { await page.locator('.bar .icon').first().click(); await page.locator('.foot button', { hasText: 'Account' }).click(); await page.waitForSelector('.big'); await sleep(1500); const b = await page.locator('.big').textContent(); await page.locator('.bar .icon').first().click(); await sleep(300); return b; };
  const num = (t) => parseFloat(String(t).replace(/[^0-9.]/g, ''));
  const before = num(await balance());
  await ctl('__set?losecall=1');
  await say('Use run_code with Python. Find a cheap search tool: found = await tools.search("google search", limit=5). Pick the cheapest with price_usd under 0.05, inspect it, call it with a simple query for "paris", print the cost_usd and ok fields. Spend as little as possible.');
  await idle();
  const calls = (await tools()).filter((c) => c.path.endsWith('/call'));
  const ids = calls.map((c) => c.body.request_id);
  ok(calls.length >= 2 && ids[0] && ids[0] === ids[1], `a lost paid answer is asked for again with the same id (${calls.length} requests)`);
  t = await text();
  ok(/cost_usd|0\.0/.test(t) && !/Can't reach|ToolError/.test(t), 'and the answer arrives: ' + t.slice(-100).replace(/\s+/g, ' '));
  const cost = num(await page.locator('.run-cost').last().textContent());
  await ctl('__set');
  await sleep(4000);
  const after = num(await balance());
  ok(cost > 0 && before - after < cost * 1.5 + 0.02, `charged once: cost $${cost}, balance $${before} -> $${after}`);
}
console.log('all passed');
process.exit(0);
