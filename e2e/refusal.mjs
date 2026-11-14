// Claude refusals, as on the web (web-refusals.mjs in the server repo). A refusal with no text says
// "The model declined to answer this." in italics. From the second refusal of the day the server
// suggests an uncensored model: one button at the bottom right of the reply, "Ask <name> instead",
// which picks that model and asks the newest question again as a new message. Both stream readers:
// plain chat (Tools and Code off) and Code Mode (on). A plan without uncensored models gets
// "Uncensored models need Pro", which opens Account (scripted through the proxy). The suggestion
// isn't kept: after a restart only the declined text is there.
//
// Needs a dev server on the server repo's fake upstream (tests/e2e/fake-upstream.mjs: every Claude
// model refuses, the uncensored model answers "hello from nano"), freshly started so its in-memory
// refusal counts are zero, and flaky-proxy.mjs in front of it:
//
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8093 tcp:8093 && TARGET=http://127.0.0.1:8231 node flaky-proxy.mjs &
//   ACCOUNT=<Pro account> [UPSTREAM=http://127.0.0.1:8099] [SHOT=/tmp/android-refusal-suggestion.png] node refusal.mjs
import { execFileSync } from 'node:child_process';
import { launch, agentSwitches, ok, shot, sleep } from './lib.mjs';

const ACCOUNT = process.env.ACCOUNT;
if (!ACCOUNT) throw new Error('ACCOUNT=<16-digit account number> is required');
const UPSTREAM = process.env.UPSTREAM || 'http://127.0.0.1:8099';
const SHOT = process.env.SHOT || '/tmp/android-refusal-suggestion.png';
const CLAUDE = 'Anthropic: Claude Fable 5.2';
const UNCENSORED = 'deepseek/deepseek-v4.1-flash-uncensored';
const UNCENSORED_NAME = 'DeepSeek V4.1 Flash Uncensored';
const ctl = (path, body) => fetch('http://127.0.0.1:8093/' + path, body ? { method: 'POST', body: JSON.stringify(body) } : {}).then((r) => r.text());
const calls = async () => (await (await fetch(UPSTREAM + '/__calls')).json()).nano[UNCENSORED] || 0;

let { page } = await launch(true);
await ctl('__set?after=0&fail=0');
await page.locator('.login input').fill(ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await sleep(2000); // the catalogue and the plan load in the background

async function pick(search, name) {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(search);
  await page.locator('.pick', { hasText: name }).first().locator('.grow').click();
  await sleep(300);
}

/** Sends `text` and waits for the reply; returns the newest reply. */
async function ask(text) {
  const before = await page.locator('.msg.assistant, .msg.err').count();
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((n) => !document.querySelector('.send.stop') && document.querySelectorAll('.msg.assistant, .msg.err').length > n, before, { timeout: 120000 });
  await sleep(300);
  return page.locator('.msg.assistant, .msg.err').last();
}

/** The reply says it was declined, in italics. */
async function declined(reply, what) {
  const em = await reply.locator('em').allTextContents();
  ok(em.includes('The model declined to answer this.') && !(await reply.getAttribute('class')).includes('err'), `${what}: the reply says, in italics, "The model declined to answer this."`);
}

/** The one button sits at the bottom right of the reply, below everything else in it: flush right,
 *  not stretched across (a long model name makes it wide). */
async function bottomRight(reply, btn) {
  const [r, b, w] = [await reply.boundingBox(), await btn.boundingBox(), await page.evaluate(() => window.innerWidth)];
  const others = await reply.evaluate((el, btn) => [...el.children].filter((c) => !c.contains(btn)).map((c) => c.getBoundingClientRect().bottom), await btn.elementHandle());
  ok(Math.abs(b.x + b.width - (r.x + r.width)) <= 2 && b.x > r.x + 16 && b.x + b.width / 2 > w / 2 && others.every((bottom) => b.y >= bottom - 1) && b.y + b.height >= r.y + r.height - 2,
    `the button is at the bottom right of the reply (button ${Math.round(b.x)}..${Math.round(b.x + b.width)} of ${w}, y ${Math.round(b.y)})`);
}

/** Taps "Ask … instead": the model changes, the question goes again, the new model answers. */
async function askInstead(reply, question, what) {
  const btn = reply.locator('.refusal-suggestion button');
  const users = await page.locator('.msg.user').count();
  const n = await calls();
  await btn.click();
  await page.waitForFunction((u) => !document.querySelector('.send.stop') && document.querySelectorAll('.msg.user').length > u
    && /hello from nano/.test(document.querySelector('#msgs > .msg:last-child')?.textContent || ''), users, { timeout: 120000 });
  ok((await page.locator('.title.model').textContent()).includes(UNCENSORED_NAME), `${what}: tapping it picks ${UNCENSORED_NAME}`);
  ok(await page.locator('.msg.user').count() === users + 1 && (await page.locator('.msg.user').last().textContent()) === question, `${what}: the question goes again as a new message`);
  ok((await page.locator('.msg.assistant').last().textContent()).includes('hello from nano') && await calls() === n + 1, `${what}: the new model answers "hello from nano"`);
  ok(await page.locator('.refusal-suggestion').count() === 0, `${what}: the button is gone`);
}

// Plain chat: the first refusal of the day, then the second.
await pick('fable', CLAUDE);
await agentSwitches(page, false);
let reply = await ask('First question for Claude');
await declined(reply, 'plain chat, first refusal');
ok(await page.locator('.refusal-suggestion').count() === 0, 'plain chat, first refusal: no button');

reply = await ask('Second question for Claude');
await declined(reply, 'plain chat, second refusal');
let btn = reply.locator('.refusal-suggestion button');
ok(await page.locator('.refusal-suggestion button').count() === 1 && (await btn.textContent()) === `Ask ${UNCENSORED_NAME} instead`,
  `plain chat, second refusal: one button, "Ask ${UNCENSORED_NAME} instead"`);
ok((await reply.locator('.refusal-suggestion').textContent()) === await btn.textContent(), 'nothing but the button: no text or info panel');
await bottomRight(reply, btn);
execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOT}`]);
ok(true, 'screenshot of the button: ' + SHOT);
await askInstead(reply, 'Second question for Claude', 'plain chat');

// Code Mode reads its stream separately: the same there.
await pick('fable', CLAUDE);
await agentSwitches(page, true);
await page.locator('.bar .icon').last().click();
await sleep(300);
reply = await ask('Third question for Claude');
await declined(reply, 'Code Mode');
btn = reply.locator('.refusal-suggestion button');
ok(await page.locator('.refusal-suggestion button').count() === 1 && (await btn.textContent()) === `Ask ${UNCENSORED_NAME} instead`, 'Code Mode: the button is there');
await bottomRight(reply, btn);
shot('refusal-code-mode');
await askInstead(reply, 'Third question for Claude', 'Code Mode');

// A plan without uncensored models: the button opens the plans.
await pick('fable', CLAUDE);
await ctl('__script', [{ refusal: true, suggest: { locked: true, model: UNCENSORED, name: UNCENSORED_NAME } }]);
reply = await ask('Fourth question for Claude');
await declined(reply, 'locked');
btn = reply.locator('.refusal-suggestion button');
ok(await page.locator('.refusal-suggestion button').count() === 1 && (await btn.textContent()) === 'Uncensored models need Pro', 'locked: one button, "Uncensored models need Pro"');
await bottomRight(reply, btn);
shot('refusal-locked');
await btn.click();
await page.waitForSelector('text=Plan', { timeout: 10000 });
ok(await page.locator('.title', { hasText: 'Account' }).count() === 1, 'locked: tapping it opens Account, with the plans');

// The declined text is kept with the chat; the suggestion isn't.
({ page } = await launch(false));
await page.waitForSelector('.msg.assistant', { timeout: 30000 });
const texts = await page.locator('.msg.assistant').allTextContents();
ok(texts.some((t) => t.includes('The model declined to answer this.')) && await page.locator('.refusal-suggestion').count() === 0,
  'after a restart the chat still says the model declined, and there is no button');
console.log('all ok');
process.exit(0);
