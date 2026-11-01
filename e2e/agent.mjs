// The agent runtime. With flaky-proxy.mjs running (see there), model replies scripted so it's free:
//   ACCOUNT=<funded number> node e2e/agent.mjs
// Long runs; every request a byte prefix of the next; compaction at the threshold and on overflow,
// through a kill, Continue and a new message; Edit drops it; no spend limit; DSML tool calls; errors
// back to the model; a run past the old 180s
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

// 9. No spend limit: nothing to set, the spend just adds up and never stops a reply.
await fresh();
ok(await page.locator('.limit').count() === 0, 'there is no limit control');
await reset();
await script([run(1, { cost: 3 }), run(2, { cost: 3 }), run(3, { cost: 3 }), { text: 'Spent freely.', usage: { cost: 1 } }]);
await say('Spend.');
await idle();
reqs = await log();
ok(reqs.length === 4, `spending past the old $5 doesn't stop it (${reqs.length} requests)`);
ok(await page.locator('.interrupted').count() === 0 && (await page.locator('.msg').last().textContent()).includes('Spent freely'), 'it finishes');
ok((await page.locator('.spend').last().textContent()).trim() === 'Spent $10.00', 'it shows what was spent');
shot('agent-spent');

// 9b. DeepSeek's own markup for a tool call runs as the call it meant, with the words before it kept.
const dsml = (code) => `Checking.\n<\uff5cDSML\uff5cfunction_calls>\n<\uff5cDSML\uff5cinvoke name="run_code">\n<\uff5cDSML\uff5cparameter name="code" string="true">${code}</\uff5cDSML\uff5cparameter>\n<\uff5cDSML\uff5cparameter name="language" string="true">python</\uff5cDSML\uff5cparameter>\n</\uff5cDSML\uff5cinvoke>\n</\uff5cDSML\uff5cfunction_calls>`;
await fresh();
await reset();
await script([{ text: dsml('print("<b>", 6*7)') }, { text: 'It is 42.' }]);
await say('Use markup.');
await idle();
reqs = await log();
const called = reqs[1].messages.at(-2);
ok(JSON.parse(called.tool_calls[0].function.arguments).code === 'print("<b>", 6*7)', 'a DSML call becomes a real call');
ok(called.content === 'Checking.\n', 'the words before it stay');
ok(String(reqs[1].messages.at(-1).content).includes('<b> 42'), 'and it really ran');
let screen = await page.locator('.msg').last().textContent();
ok(!screen.includes('DSML') && screen.includes('It is 42'), 'no markup on screen');

// 9c. A tool call nobody can read goes back to the model to redo.
await fresh();
await reset();
await script([{ text: '<\uff5cDSML\uff5cfunction_calls>garbled' }, { text: 'Now properly.' }]);
await say('Garble it.');
await idle();
reqs = await log();
ok(reqs.length === 2 && String(reqs[1].messages.at(-1).content).includes('written out as text'), 'an unreadable call goes back to the model');
screen = await page.locator('.msg').last().textContent();
ok(!screen.includes('DSML') && screen.includes('Now properly'), 'and the screen shows only the answer');

// 9d. A request that keeps failing goes back to the model instead of ending the reply.
await fresh();
await reset();
const bad = { error: 'Invalid tool schema in request' };
await script([run(1), bad, bad, bad, { text: 'Adjusted and done.' }]);
await say('Fail then fix.');
await idle();
reqs = await log();
ok(reqs.length === 5, `it retries, then tells the model (${reqs.length} requests)`);
ok(String(reqs[4].messages.at(-1).content).includes('Invalid tool schema'), 'the model sees the error');
ok(await page.locator('.interrupted').count() === 0 && (await page.locator('.msg').last().textContent()).includes('Adjusted and done'), 'and the reply goes on');

// 9d2. Tools busy (our balance waiting on a top-up): a paid call waits and delivers, charged once.
await fresh();
await ctl('__set?toolbusy=3');
await script([{ code: 'r = await tools.call("m:dataforseo/labs/ranked-keywords", {"target": "x"}, {"max_cost_usd": 0.05})\nprint("paid", r["cost_usd"])' }, { text: 'Delivered.' }]);
await say('Busy tools.');
await idle();
reqs = await log();
const toolReqs = JSON.parse(await ctl('__tools')).filter((t) => t.path.endsWith('/call'));
ok(toolReqs.length === 3 && String(reqs[1].messages.at(-1).content).includes('paid 0.02'), `a busy tool waits and delivers (${toolReqs.length} calls)`);
ok(new Set(toolReqs.map((t) => t.body.request_id)).size === 1, 'with the same request id, so it runs once');
ok(toolReqs[0].body.max_cost_usd === 0.05, 'and the {max_cost_usd} form sets the cap');

// 9e. A proxy's error page while the server restarts is a wait, not a failure.
await fresh();
await reset();
await script([run(1), { status: 502 }, { status: 503 }, { status: 502 }, { status: 504 }, { text: 'After the restart.' }]);
await say('Restart.');
await idle();
reqs = await log();
ok(reqs.length === 6 && await page.locator('.interrupted').count() === 0, `it waits out the restart (${reqs.length} requests)`);

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
// Screen off and deep Doze: what a phone in a pocket does, and where Android cuts off apps' network.
adb('shell', 'input', 'keyevent', 'KEYCODE_SLEEP');
adb('shell', 'dumpsys', 'battery', 'unplug');
adb('shell', 'dumpsys', 'deviceidle', 'force-idle');
// Android holds this notification back for about 10 seconds, so a quick reply never shows it.
let during = false;
for (let i = 0; i < 15 && !during; i++) { await sleep(1000); during = adb('shell', 'dumpsys', 'notification', '--noredact').includes('Obscurify is working'); }
ok(during, 'a notification says it is working');
const until = Date.now() + 270000;
let finished = false;
while (Date.now() < until && !finished) {
  await sleep(5000);
  finished = (await log()).length === 3 && adb('shell', 'dumpsys', 'notification', '--noredact').includes('Your answer is ready');
}
ok(adb('shell', 'dumpsys', 'deviceidle', 'get', 'deep') === 'IDLE', 'the phone was in deep Doze the whole time');
adb('shell', 'dumpsys', 'deviceidle', 'unforce');
adb('shell', 'dumpsys', 'battery', 'reset');
adb('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP');
ok(finished, 'the 200 second run finished in the background and the next steps ran');
reqs = await log();
ok(JSON.stringify(reqs[1].messages).includes('woke'), 'the run outlived the old limit, with its output');
shot('agent-notified');
ok(!adb('shell', 'dumpsys', 'notification', '--noredact').includes('Obscurify is working'), 'the working notification is gone');
adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`);
await sleep(2000);
ok((await page.locator('.msg').last().textContent()).includes('Done in the background'), 'the answer is there on return');

// 11b. Plain chat keeps going in the background too. No crash on a reply that ends at once.
await page.locator('.tools-switch').click();
await fresh();
await reset();
await script([{ text: 'Plain and quick.' }]);
await say('Plain.');
await idle();
ok((await page.locator('.msg').last().textContent()).includes('Plain and quick'), 'a quick plain reply starts and stops the service cleanly');
await page.locator('.tools-switch').click();

// 12. OUTAGE_MS=960000: longer than the old 15 minute cap, and it still delivers by itself.
if (process.env.OUTAGE_MS) {
  await fresh();
  await reset();
  await script([run(1), { text: 'Waited it out.' }]);
  await ctl(`__set?chatdown=${process.env.OUTAGE_MS}`);
  const t0 = Date.now();
  await say('Long outage.');
  await page.waitForSelector('.waiting', { timeout: 60000 });
  await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: +process.env.OUTAGE_MS + 120000 });
  ok(Date.now() - t0 >= +process.env.OUTAGE_MS, 'it was down the whole time');
  ok(await page.locator('.interrupted').count() === 0 && (await page.locator('.msg').last().textContent()).includes('Waited it out'), 'and it delivered by itself, no Continue');
}

console.log('agent runtime ok');
process.exit(0);
