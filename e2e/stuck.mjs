// A busy paid tool, or an answer that goes silent with the connection open, never holds a chat
// until the app is restarted (customer report 2026-11-10, on the web: a tool answered "busy" for
// minutes and the chat waited up to 15 minutes per call).
//  1. A paid call that answers 429 for good is waited out for two minutes, then the run hears it.
//  2. Code Mode: an answer that goes silent is asked again after a minute and the turn finishes.
//  3. Plain chat: an answer that goes silent ends with an error after a minute.
// With flaky-proxy.mjs running (see there): ACCOUNT=<funded number> node e2e/stuck.mjs
import { launch, agentSwitches, ok, sleep } from './lib.mjs';

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

// 1. Tools busy for good.
await ctl('__set?toolbusy=-1');
await script([
  { code: 'try:\n    r = await tools.call("m:litescrape/web/fetch", {"body": {"url": "https://example.com"}})\n    print("GOT", r)\nexcept Exception as e:\n    print("ERR", e)' },
  { text: 'The tool was busy, so I could not fetch it.' },
]);
await say('Fetch example.com with a paid tool.');
let r = await untilIdle(300);
ok(r.done && r.longest <= 170, `a paid tool that stays busy is waited out for about two minutes, not fifteen (${JSON.stringify(r)})`);
ok(/busy/i.test(JSON.stringify(JSON.parse(await ctl('__log')).at(-1).messages)), 'the run heard the tool was busy');
ok(!(await page.locator('.waiting').count()), 'no waiting note is left');

// 2. Code Mode: the first answer goes silent, the next one is scripted.
await ctl('__set?silent=1');
await script([{ text: 'It is 391.' }]);
await say('What is 17 * 23?');
r = await untilIdle(200);
ok(r.done && /391/.test(await last()), `Code Mode: a silent answer is asked again and the turn finishes (${JSON.stringify(r)})`);

// 3. Plain chat (Tools and Code off): a silent answer ends with an error.
await agentSwitches(page, false);
await ctl('__set?silent=1');
await say('Hello');
r = await untilIdle(150);
ok(r.done && r.seconds <= 90, `plain chat: a silent answer ends instead of waiting for good (${JSON.stringify(r)})`);
ok(/connection dropped|isn't answering|Obscurify/i.test(await last()), 'it says the connection dropped');
await agentSwitches(page, true);

console.log('stuck: all passed');
process.exit(0);
