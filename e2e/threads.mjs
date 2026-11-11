// Conversations held on the server for up to an hour: after the first request the app sends only
// what's new, and the model still gets all of it. Real model, through flaky-proxy.mjs to a server:
//   TARGET=https://<server> node flaky-proxy.mjs &   (test APK with OBSCURIFY_API=http://127.0.0.1:8093,
//   adb reverse tcp:8093 tcp:8093)
//   ACCOUNT=<funded number> RESTART="<command that restarts the server>" node e2e/threads.mjs
import { execSync } from 'node:child_process';
import { launch, ok, sleep, flip } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const log = async () => JSON.parse(await ctl('__log'));
const reset = () => ctl('__set?after=0&fail=0');
const said = (b, text) => JSON.stringify(b.messages).includes(text);
const FIRST = 'Remember the code word';
// Whether `b` sends the system prompt or the user message holding `text` again (a model's own reasoning may quote it).
const resends = (b, text) => b.messages.some((m) => m.role === 'system' || (m.role === 'user' && JSON.stringify(m).includes(text)));

let { page } = await launch(true);
await reset();
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const say = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await idle();
  await sleep(300);
};
const answer = async () => (await page.locator('.msg').last().textContent()) || '';

// 1. Code Mode: a turn with a tool call, then another turn.
await say(`${FIRST} PELICAN-42. Use run_code with JavaScript to print 6*7, then reply with the number only.`);
let reqs = await log();
ok(reqs[0].thread && !reqs[0].thread.rev && said(reqs[0], FIRST), 'the first request starts a thread with the whole conversation');
ok(reqs.length >= 2 && reqs.slice(1).every((b) => b.thread && b.thread.rev && !resends(b, FIRST) && b.messages.some((m) => m.role === 'tool')),
  'each step sends only its new messages: ' + reqs.map((b) => b.messages.map((m) => m.role).join('+')).join(' | '));
ok((await answer()).includes('42'), 'the model saw the tool result');

await reset();
await say('What was the code word? Reply with the code word only, no tools.');
reqs = await log();
ok(reqs[0].thread.rev && reqs[0].messages.length <= 2 && !resends(reqs[0], FIRST), 'the next turn sends only the new question');
ok((await answer()).includes('PELICAN'), 'and the model still has the whole conversation');

// 2. The server restarts (a deploy): it no longer holds the conversation, so it goes whole once.
if (process.env.RESTART) {
  execSync(process.env.RESTART);
  for (let i = 0; i < 60; i++) { try { if ((await fetch('http://127.0.0.1:8093/health')).ok) break; } catch {} await sleep(1000); }
  await reset();
  await say('Say the code word once more, nothing else.');
  reqs = await log();
  ok(reqs.length >= 2 && reqs[0].thread.rev && !reqs[1].thread.rev && said(reqs[1], FIRST), 'after a restart the rev is refused and the whole conversation goes again');
  ok((await answer()).includes('PELICAN'), 'and the reply still works');
}

// 3. Plain chat (tools off) holds its conversation the same way.
await page.locator('.bar .icon').last().click();
await sleep(300);
await flip(page, '.tools-switch');
await reset();
await say('Remember the number 9137. Reply OK.');
await say('Which number? Number only.');
reqs = await log();
ok(reqs.length === 2 && reqs[1].thread.rev && !resends(reqs[1], '9137') && !reqs[1].tools, 'plain chat: the second turn sends only what is new');
ok((await answer()).includes('9137'), 'plain chat: the model has the whole conversation');
await flip(page, '.tools-switch');
console.log('all ok');
process.exit(0);
