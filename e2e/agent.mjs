// The agent runtime. With flaky-proxy.mjs running (see there), model replies scripted so it's free:
//   ACCOUNT=<funded number> node e2e/agent.mjs
// Long runs; every request a byte prefix of the next; compaction at the threshold and on overflow,
// through a kill, Continue and a new message; Edit drops it; the spend limit; a run past the old 180s
// with the app in the background, delivering a notification.
import { launch, ok, sleep, adb, shot, PKG } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const log = async () => JSON.parse(await ctl('__log'));
const reset = () => ctl('__set?after=0&fail=0');
const script = (s) => fetch('http://127.0.0.1:8093/__script', { method: 'POST', body: JSON.stringify(s) });
const CONTEXT = 1048576; // DeepSeek V4 Flash, from /models
const HIGH = Math.ceil(CONTEXT * 0.7) + 1;
const LONG = 'S'.repeat(300);
const run = (n, usage = {}) => ({ code: `print("step", ${n})`, usage });

let { page } = await launch(true);
await reset();
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { timeout: 30000 });
const chooseModel = async () => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill('DeepSeek V4 Flash');
  await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
};
await chooseModel();
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const say = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await sleep(300);
};
const fresh = async () => { await page.locator('.bar .icon').last().click(); await sleep(300); };
const isPrefix = (a, b) => {
  const x = JSON.stringify(a.messages), y = JSON.stringify(b.messages);
  return y.startsWith(x.slice(0, -1));
};
const allPrefix = (reqs) => reqs.slice(1).every((r, i) => isPrefix(reqs[i], r));
const summaryOf = (r) => String(r.messages[1]?.content);

// 1. Past the old 10 steps, append-only, usage asked for.
await reset();
await script([...Array(14).keys()].map((n) => run(n)).concat({ text: 'All fourteen done.' }));
await say('Run the steps.');
await idle();
let reqs = await log();
ok(reqs.length === 15, `a reply runs past the old 10 steps (${reqs.length} requests)`);
ok(allPrefix(reqs), "every step's messages are a byte prefix of the next step's");
ok(reqs.every((r) => r.stream_options?.include_usage === true), 'every step asks for usage');
ok(await page.locator('.run').count() === 14, 'all the runs are on screen');

// 2. Compaction at the threshold.
await fresh();
await reset();
await script([run(1), run(2), run(3, { prompt_tokens: HIGH }), { text: `SUMMARY-ONE ${LONG}` }, run(4), run(5), { text: 'Finished after compacting.' }]);
await say('Compact this work.');
await idle();
reqs = await log();
ok(reqs.length === 7, `the summary is one extra request (${reqs.length})`);
const before = reqs[2], ask = reqs[3], after = reqs.slice(4);
ok(isPrefix(before, ask), 'the summary request keeps the exact prefix, so it reads from the cache');
ok(JSON.stringify(ask.tools) === JSON.stringify(before.tools) && !ask.tool_choice, 'and the same tools, with no tool_choice');
ok(ask.messages.at(-1).content.startsWith('Summarize this conversation') && JSON.stringify(ask.messages.slice(0, -1)).includes('step'), 'it asks for the summary, with all the work in front of it');
const first = after[0].messages;
ok(first[0].role === 'system' && first[0].content === before.messages[0].content, 'the system prompt is unchanged');
ok(first[1].role === 'user' && first[1].content.includes('SUMMARY-ONE'), 'then the summary');
ok(first[2].role === 'assistant' && first[2].content === 'Understood. Continuing.', 'then the acknowledgment');
ok(first[3].role === 'user' && first[3].content === 'Compact this work.', 'then the request, word for word');
ok(first[4]?.role === 'assistant' && first[4].tool_calls?.length, 'then the verbatim tail, starting on a step');
ok(first.every((m, i) => i === 0 || !(m.role === 'assistant' && first[i - 1].role === 'assistant')), 'never two assistant messages in a row');
const callIds = new Set(first.flatMap((m) => (m.tool_calls || []).map((c) => c.id)));
ok(first.filter((m) => m.role === 'tool').every((m) => callIds.has(m.tool_call_id)), 'no tool result without its call');
ok(first.length < before.messages.length + 2 || !JSON.stringify(first).includes('"step", 1'), 'the older steps are gone from the request');
ok(allPrefix(after), 'append-only again afterwards');
ok(await page.locator('.run').count() === 5, 'nothing disappears from the screen');
ok(await page.locator('.compacted').count() === 1, 'a small line says it happened');
shot('agent-compacted');

// 3. A new message goes on from the summary.
await reset();
await script([{ text: 'Second answer.' }]);
await say('And now?');
await idle();
reqs = await log();
ok(summaryOf(reqs[0]).includes('SUMMARY-ONE') && JSON.stringify(reqs[0].messages).includes('Finished after compacting'), 'a new message sends the summary and what came after');
ok(isPrefix(after.at(-1), { messages: reqs[0].messages }), 'and keeps the prefix of the last request');

// 4. Tools off, plain chat honors it too.
await page.locator('.tools-switch').click();
await reset();
await script([{ text: 'Plain answer.' }]);
await say('Plain question');
await idle();
reqs = await log();
ok(String(reqs[0].messages[0].content).includes('SUMMARY-ONE') && !reqs[0].tools, 'plain chat sends the summary too');
await page.locator('.tools-switch').click();

// 5. Reopening keeps it: kill the app, send again.
({ page } = await launch(false));
await page.waitForSelector('.tools-switch', { timeout: 30000 });
await page.locator('.bar .icon').first().click();
await sleep(500);
await page.locator('.drawer .item').filter({ hasText: 'Compact this work' }).first().locator('.grow').click();
await sleep(500);
await chooseModel();
await reset();
await script([{ text: 'After reload.' }]);
await say('After a restart?');
await idle();
reqs = await log();
ok(summaryOf(reqs[0]).includes('SUMMARY-ONE'), 'the compaction survives a restart');

// 6. Edit back before it drops it.
await reset();
await page.locator('.msg', { hasText: 'Finished after compacting' }).getByRole('button', { name: 'Edit' }).first().click();
await sleep(400);
await script([{ text: 'Edited answer.' }]);
await page.locator('.send').click();
await sleep(300);
await idle();
reqs = await log();
ok(!JSON.stringify(reqs[0].messages).includes('SUMMARY-ONE'), 'editing back before it drops the summary');

// 7. A summary that fails keeps appending, and the next boundary tries again.
await fresh();
await reset();
await script([run(1, { prompt_tokens: HIGH }), { text: 'too short' }, run(2, { prompt_tokens: HIGH }), { text: `SUMMARY-TWO ${LONG}` }, { text: 'Done.' }]);
await say('Fail the summary once.');
await idle();
reqs = await log();
ok(reqs.length === 5, `a failed summary tries again after the next step (${reqs.length})`);
ok(isPrefix(reqs[0], reqs[2]), 'it kept appending');
ok(summaryOf(reqs[4]).includes('SUMMARY-TWO'), 'and the second one took');

// 8. Too long for the model: compacts once, from a reduced copy, and asks again.
await fresh();
await reset();
await script([{ code: `print("x" * 9000)` }, { error: "This model's maximum context length is 100 tokens." }, { text: `SUMMARY-THREE ${LONG}` }, { text: 'Recovered.' }]);
await say('Overflow.');
await idle();
reqs = await log();
ok(reqs.length === 4, `overflow goes to a summary and retries (${reqs.length})`);
const toolIn = (r) => r.messages.filter((m) => m.role === 'tool').map((m) => m.content.length);
ok(Math.max(...toolIn(reqs[1])) > 8000 && Math.max(...toolIn(reqs[2])) <= 1600, 'the summary request is reduced');
ok(summaryOf(reqs[3]).includes('SUMMARY-THREE'), 'and the retry sends the summary');
ok(await page.locator('.interrupted').count() === 0 && (await page.locator('.msg').last().textContent()).includes('Recovered'), 'and the answer arrives');

// 9. The spend limit: set it, stop at it, Continue grants another.
await fresh();
await page.locator('.limit').click();
await page.locator('.limit-input').fill('0.05');
await page.locator('.limit-input').blur();
await sleep(400);
ok((await page.locator('.limit').textContent()).includes('0.05'), 'the limit can be set');
await reset();
await script([run(1, { cost: 0.03 }), run(2, { cost: 0.03 }), run(3, { cost: 0.03 }), { text: 'Under the new limit.', usage: { cost: 0.01 } }]);
await say('Spend.');
await idle();
reqs = await log();
ok(reqs.length === 2, `it stops at the limit (${reqs.length} requests)`);
const stopped = await page.locator('.interrupted').textContent();
ok(stopped.includes('Spend limit of $0.05 reached'), 'it says why: ' + stopped);
ok((await page.locator('.spend').last().textContent()).includes('$0.06'), 'it shows the spend');
shot('agent-limit');
await page.getByRole('button', { name: 'Continue' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
reqs = await log();
ok(reqs.length === 4, `Continue grants another limit's worth (${reqs.length})`);
ok((await page.locator('.msg').last().textContent()).includes('Under the new limit'), 'and it finishes');
ok((await page.locator('.spend').last().textContent()).includes('$0.10'), 'the spend adds up across it');
({ page } = await launch(false));
await page.waitForSelector('.limit', { timeout: 30000 });
ok((await page.locator('.limit').textContent()).includes('0.05'), 'the limit persists');
await page.locator('.limit').click();
await page.locator('.limit-input').fill('5');
await page.locator('.limit-input').blur();
await sleep(300);

// 10. Killed mid-run after a compaction: comes back, Continue goes on from the summary.
await fresh();
await chooseModel();
await reset();
await script([run(1, { prompt_tokens: HIGH }), { text: `SUMMARY-FOUR ${LONG}` }, { code: 'import time\nprint("slow")\ntime.sleep(25)' }]);
await say('Die after compacting.');
await page.waitForFunction(() => document.body.textContent.includes('Compacted earlier work'), null, { timeout: 60000 });
await sleep(1500);
({ page } = await launch(false));
await page.waitForSelector('.tools-switch', { timeout: 30000 });
await page.locator('.bar .icon').first().click();
await sleep(500);
await page.locator('.drawer .item').filter({ hasText: 'Die after compacting' }).first().locator('.grow').click();
await sleep(500);
await chooseModel();
await reset();
await script([{ text: 'Back from the dead.' }]);
await page.getByRole('button', { name: 'Continue' }).click();
await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
await idle();
reqs = await log();
ok(summaryOf(reqs[0]).includes('SUMMARY-FOUR'), 'Continue after a kill sends the summary');
ok(reqs[0].messages.some((m) => m.role === 'assistant' && m.tool_calls), 'with the tail');

// 11. Past 180s with the app in the background: it keeps going and says when it's ready.
await fresh();
await reset();
await script([{ code: 'import time\nprint("start")\ntime.sleep(200)\nprint("woke")' }, run(2), { text: 'Done in the background.' }]);
await say('Long run.');
await sleep(4000);
adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
await sleep(3000);
const during = adb('shell', 'dumpsys', 'notification', '--noredact');
ok(during.includes('Obscurify is working'), 'a notification says it is working');
const until = Date.now() + 270000;
let finished = false;
while (Date.now() < until && !finished) {
  await sleep(5000);
  finished = (await log()).length === 3 && adb('shell', 'dumpsys', 'notification', '--noredact').includes('Your answer is ready');
}
ok(finished, 'the 200 second run finished in the background and the next steps ran');
reqs = await log();
ok(JSON.stringify(reqs[1].messages).includes('woke'), 'the run outlived the old limit, with its output');
shot('agent-notified');
ok(!adb('shell', 'dumpsys', 'notification', '--noredact').includes('Obscurify is working'), 'the working notification is gone');
adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`);
await sleep(2000);
ok((await page.locator('.msg').last().textContent()).includes('Done in the background'), 'the answer is there on return');

console.log('agent runtime ok');
process.exit(0);
