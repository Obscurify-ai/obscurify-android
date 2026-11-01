// A failure never throws away tool work. With flaky-proxy.mjs running (see there):
//   ACCOUNT=<funded number> node e2e/resume.mjs
// Failed steps retry by themselves; then the answer stops, keeps its runs and offers Continue; a new
// message carries the tool results too; Retry doesn't redo paid work; a killed app comes back; and
// selecting text offers Android's Copy toolbar.
import { launch, ok, sleep, adb, shot } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const set = (after, fail, kind = 'error') => ctl(`__set?after=${after}&fail=${fail}&kind=${kind}`);
const log = async () => JSON.parse(await ctl('__log'));
const PROMPT = 'Use run_code with Python: print("value", 1234*5678). Then tell me the printed value.';
const hasTool = (body, text) => body.messages.some((m) => m.role === 'tool' && String(m.content).includes(text));

let { page } = await launch(true);
await set(0, 0);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { timeout: 30000 });
const chooseModel = async () => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill('DeepSeek V4 Flash');
  await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
};
await chooseModel();
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 170000 });
const say = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await sleep(300);
};
const fresh = async () => { await page.locator('.bar .icon').last().click(); await sleep(300); };

// 1. Your error, over and over: retries, then stops with everything kept.
await set(1, 99);
await say(PROMPT);
await idle();
let reqs = await log();
ok(reqs.length === 4, `a failed step retries twice by itself (${reqs.length} requests)`);
ok(hasTool(reqs[3], '7006652'), 'the retries carry the tool result: nothing runs again');
const note = await page.locator('.interrupted').textContent();
ok(note.includes('partial tool call received') && note.includes('kept'), 'it says what happened: ' + note);
ok(await page.locator('.run').count() === 1, 'the run is still on screen');
shot('resume-stopped');

// 2. Continue picks up from there.
await set(0, 0);
await page.getByRole('button', { name: 'Continue' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
reqs = await log();
ok(hasTool(reqs[0], '7006652'), 'Continue sends the tool result it already has');
ok(await page.locator('.run').count() === 1 && reqs.length === 1, 'the code is not run again');
ok((await page.locator('.msg.assistant').last().textContent()).replace(/,/g, '').includes('7006652'), 'and the answer finishes');
ok(await page.locator('.interrupted').count() === 0, 'the note is gone');

// 3. Retry on a finished tool answer reuses the results too.
await set(0, 0);
await page.locator('.meta button', { hasText: 'Retry' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
reqs = await log();
ok(hasTool(reqs[0], '7006652') && await page.locator('.run').count() === 1, 'Retry keeps the tool results and does not pay again');

// 4. Sending another message instead of Continue keeps it all in the conversation.
await fresh();
await set(1, 99);
await say(PROMPT);
await idle();
await page.waitForSelector('.interrupted');
await set(0, 0);
await say('What value did the code print? Reply with only the number, do not run anything.');
await idle();
reqs = await log();
ok(hasTool(reqs[0], '7006652'), 'a new message carries the earlier tool result');
ok(await page.locator('.msg.user').count() === 2 && await page.locator('.run').count() === 1, 'the stopped answer stays in the chat');
ok((await page.locator('.msg.assistant').last().textContent()).replace(/,/g, '').includes('7006652'), 'the model knows the result');

// 5. One hiccup: the user never notices.
await fresh();
await set(1, 1);
await say(PROMPT);
await idle();
ok(await page.locator('.interrupted').count() === 0 && (await page.locator('.msg.assistant').last().textContent()).replace(/,/g, '').includes('7006652'), 'a single failure heals by itself');

// 6. The connection drops in the middle of an answer: not taken for a finished one.
await fresh();
await set(0, 99, 'cut');
await say('Write three long paragraphs about the sea. Do not use any tools.');
await idle();
ok((await page.locator('.interrupted').textContent()).includes('connection dropped'), 'a cut-off answer says so');
await set(0, 0);
await page.getByRole('button', { name: 'Continue' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
ok(await page.locator('.interrupted').count() === 0, 'and Continue finishes it');

// 7. The app is killed while the model is working: it comes back with the run.
await fresh();
await set(1, 1, 'hang');
await say(PROMPT);
for (let i = 0; i < 120 && (await log()).length < 2; i++) await sleep(1000);
ok((await log()).length === 2, 'the run finished and the next step is hanging');
await sleep(500);
({ page } = await launch(false));
await page.waitForSelector('.interrupted', { timeout: 30000 });
ok((await page.locator('.interrupted').textContent()).includes('closed') && await page.locator('.run').count() === 1, 'a killed app comes back with the run and Continue');
await set(0, 0);
await page.waitForSelector('.tools-switch', { timeout: 20000 });
await page.getByRole('button', { name: 'Continue' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
reqs = await log();
ok(hasTool(reqs[0], '7006652') && (await page.locator('.msg.assistant').last().textContent()).replace(/,/g, '').includes('7006652'), 'and finishes without running it again');

// 8. Selecting text offers Copy.
const b = await page.locator('.msg.assistant p').last().boundingBox();
const dpr = await page.evaluate(() => devicePixelRatio);
const top = +adb('shell', 'dumpsys', 'window', 'displays').match(/statusBars[^\n]*?frame=\[0,0\]\[\d+,(\d+)\]/)?.[1] || 147;
const [x, y] = [Math.round((b.x + 40) * dpr), Math.round((b.y + 12) * dpr + top)];
adb('shell', 'input', 'swipe', String(x), String(y), String(x), String(y), '900');
await sleep(2000);
// The first dumps can come back before the toolbar's window is in them.
let toolbar = false;
for (let i = 0; i < 5 && !toolbar; i++) {
  adb('shell', 'uiautomator', 'dump', '--windows', '/sdcard/ui.xml');
  toolbar = /Select all|Read aloud/.test(adb('shell', 'cat', '/sdcard/ui.xml'));
  await sleep(700);
}
shot('resume-copy');
ok(toolbar, 'selecting text brings up the Copy toolbar');
console.log('all passed');
process.exit(0);
