// Background replies: a signed-in answer runs on the server whatever happens to the app. A dropped
// connection picks up from the last whole event; an app killed mid-answer finishes it into its chat
// when it opens again (a plain answer and a Code Mode step, whose run then carries on); Stop ends it
// on the server too. Real model, through flaky-proxy.mjs to a server:
//   TARGET=http://127.0.0.1:8241 node flaky-proxy.mjs &   (test APK with OBSCURIFY_API=http://127.0.0.1:8093,
//   adb reverse tcp:8093 tcp:8093)
//   ACCOUNT=<funded number> node e2e/background.mjs
import { execFileSync } from 'node:child_process';
import { adb, launch, ok, sleep, PKG } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const paths = async () => JSON.parse(await ctl('__paths'));
const SHOTS = process.env.SHOTS || `${process.env.HOME}/Work/obscurify/screenshots`;
const shot = (name) => execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/${name}.png`]);
const COUNT = 'Count from 1 to 60 in English words, one per line, nothing else.';

let { page } = await launch(true);
await ctl('__set');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
if ((await page.locator('.tools-switch').getAttribute('aria-checked')) === 'true') await page.locator('.tools-switch').click();

const idle = (p = page) => p.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const ask = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 });
};
const last = (p = page) => p.locator('.msg.assistant').last().textContent().then((t) => t || '');
// Text of the answer being written.
const writing = (p = page) => p.evaluate(() => [...document.querySelectorAll('.msg.assistant')].pop()?.textContent || '');

// 1. The connection drops after a few lines: the answer is picked up again and arrives whole.
await ctl('__set?fail=1&kind=cut');
await ask(COUNT);
await idle();
let a = await last();
ok(/sixty/i.test(a) && !/dropped/i.test(a), 'a dropped connection is picked up and the answer arrives whole');
ok((await paths()).some((p) => /GET \/web\/replies\/rep_/.test(p)), 'through /web/replies');
await ctl('__set');

// 2. The app is killed mid-answer: opening it again finishes the answer into its chat.
await ctl('__set');
await ask(COUNT.replace('60', '80'));
await page.waitForFunction(() => /\bone\b/i.test([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || ''), null, { timeout: 60000 });
adb('shell', 'am', 'force-stop', PKG);
await sleep(20000);
({ page } = await launch(false));
await page.waitForFunction(() => /eighty/i.test([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || ''), null, { timeout: 120000 });
await idle();
a = await last();
ok(/eighty/i.test(a) && /\bone\b/i.test(a), 'a killed app finishes the answer when it opens');
ok((await paths()).some((p) => /GET \/web\/replies\/rep_/.test(p)), 'from the reply the server held');
shot('android-background-resumed');
({ page } = await launch(false));
ok(/eighty/i.test(await last()), 'and it was saved to its chat');

// 3. Stop ends the answer on the server too.
await ctl('__set');
await ask('Write a 600-word essay about tides.');
await page.waitForFunction(() => ([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || '').length > 40, null, { timeout: 60000 });
await page.locator('.send.stop').click();
await idle();
await sleep(1500);
ok((await paths()).some((p) => /POST \/web\/replies\/rep_\w+\/cancel/.test(p)), 'Stop cancels the reply on the server');
const stopped = (await last()).length;
({ page } = await launch(false));
await sleep(3000);
ok((await last()).length <= stopped + 40, 'and a reopened app does not bring it back');

// 4. Code Mode killed mid-step: the step is picked up and the run carries on.
await ctl('__set');
await page.locator('.tools-switch').click();
await ask('First write about 150 words on why multiplication tables matter, as plain text. Only after that, use run_code with Python to print 6*7, then end with the line "Result: " and the number.');
await page.waitForFunction(() => ([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || '').length > 80, null, { timeout: 90000 });
adb('shell', 'am', 'force-stop', PKG);
await sleep(15000);
({ page } = await launch(false));
await page.waitForFunction(() => /Result:\s*42/.test([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || ''), null, { timeout: 240000 });
await idle();
a = await last();
ok(/Result:\s*42/.test(a) && !a.includes('Stopped early'), 'a Code Mode step is picked up after the app was killed, and the run carries on');
ok((await paths()).some((p) => /GET \/web\/replies\/rep_/.test(p)), 'from the reply the server held');
console.log('background: all passed');
process.exit(0);
