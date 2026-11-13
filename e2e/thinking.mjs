// The thinking section: one fold per answer, folded until opened, holding what happened in order
// (each step's reasoning, what it said, its tool calls and code). Tool calls are rows without
// emoji; a row opens to its input, its answer and its cost. Pictures a tool made stay outside the
// fold. A restart brings the same structure back. Model replies and tool answers are scripted by
// flaky-proxy.mjs, so nothing is charged:
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8093 tcp:8093 && node e2e/flaky-proxy.mjs &
//   ACCOUNT=<funded number> SHOTS=<dir> node e2e/thinking.mjs
import { execFileSync } from 'node:child_process';
import { launch, agentSwitches, ok, sleep, shot } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const post = (path, body) => fetch('http://127.0.0.1:8093/' + path, { method: 'POST', body: JSON.stringify(body) });
const PICTURE = 'https://obscurify.ai/static/images/web-app-manifest-512x512.png';
const shots = process.env.SHOTS;
const keep = (name) => { shot(name); if (shots) execFileSync('cp', [`/tmp/obs-e2e-${name}.png`, `${shots}/android-${name}.png`]); };

let { page } = await launch(true);
await ctl('__set?after=0&fail=0');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
await agentSwitches(page, true);

await post('__script', [
  { reasoning: 'R1 First I search, then make a picture.', text: 'Let me look that up.', tools: [
    { name: 'web_search', args: { query: 'bitcoin price' } },
    { name: 'call_tool', args: { id: 'acme/picture', input: { prompt: 'a cat' }, max_cost_usd: 0.05 } },
  ] },
  { reasoning: 'R2 The picture worked. Now the other tool.', tools: [{ name: 'call_tool', args: { id: 'acme/broken', input: {} } }] },
  { reasoning: 'R3 That one failed. I will work it out in Python.', code: 'import time\ntime.sleep(4)\nprint(6 * 7)' },
  { reasoning: 'R4 All done, writing the answer.', text: 'The answer is 42.' },
]);
await post('__toolscript', [
  { body: { results: 'Bitcoin is about $70,000 today.' } },
  { body: { ok: true, status: 200, output: { image: PICTURE }, cost_usd: 0.0123, balance_usd: 3 } },
  { status: 503, body: { error: 'The tool broke upstream.' } },
]);

await page.locator('#composer').fill('What is six times seven? Use the tools.');
await sleep(300);
await page.locator('.send').click();
const reply = () => page.locator('.msg.assistant').last();
const fold = () => reply().locator('details.thinking');

// While it works: folded, and the title says what it is doing.
await page.waitForSelector('.run.going', { state: 'attached', timeout: 60000 });
ok(await fold().count() === 1, 'one thinking section while it works');
ok(!(await fold().evaluate((d) => d.open)), 'folded while it works');
const liveLabel = await fold().locator(':scope > summary').textContent();
ok(liveLabel.startsWith('Thinking…'), 'its title says it is thinking: ' + liveLabel);
ok(/Running Python/.test(liveLabel), 'and what it is doing now: ' + liveLabel);
keep('thinking-live');
// Opened while it works, it stays open when the answer lands.
await fold().locator(':scope > summary').click();
await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 120000 });
ok(await fold().evaluate((d) => d.open), 'opened while it worked, it stays open when the answer is in');
await fold().locator(':scope > summary').click();
ok(!(await fold().evaluate((d) => d.open)), 'and folds again on a tap');

const label = (await fold().locator(':scope > summary').textContent()).trim();
ok(label === 'Thinking · 4 tool calls · $0.01', 'done: the title counts the tool calls and their cost: ' + label);
ok((await reply().textContent()).includes('The answer is 42.'), 'the answer is there');
ok(!(await fold().textContent()).includes('The answer is 42.'), 'the answer is outside the fold');
keep('thinking-folded');

// In order, inside the fold.
await fold().locator(':scope > summary').click();
const order = await fold().evaluate((d) => [...d.querySelectorAll('.think, .step-text, .step > .tool-call > summary, .run-head')]
  .map((n) => n.textContent.trim().replace(/\s+/g, ' ')));
console.log('   fold:', order);
const want = [/^R1 /, /^Let me look that up\.$/, /^web_search/, /^call ?acme\/picture/, /^R2 /, /^call ?acme\/broken/, /^R3 /, /^Python/, /^R4 /];
ok(order.length === want.length && want.every((w, i) => w.test(order[i])), 'reasoning, words, tool calls and code are in the order they happened');

// Rows: no emoji, cost on the row, failed in red.
const rows = fold().locator('.tool-call > summary');
const rowText = (await rows.allTextContents()).join(' | ');
ok(!/\p{Extended_Pictographic}/u.test(rowText), 'no emoji in the tool rows: ' + rowText);
const row = (t) => fold().locator('.tool-call', { has: page.locator(':scope > summary', { hasText: t }) }).first();
ok((await row('bitcoin price').locator('.tool-cost').textContent()) === 'free', 'a free call says free');
ok((await row('acme/picture').locator('.tool-cost').textContent()) === '$0.01', 'a paid call shows its cost on the row');
ok((await row('acme/broken').locator('.tool-state').textContent()) === 'failed' && await row('acme/broken').evaluate((d) => getComputedStyle(d.querySelector('.tool-state')).color) === 'rgb(255, 84, 84)', 'a failed call says failed, in red');

// A row opens to the call.
ok(!(await row('acme/picture').evaluate((d) => d.open)), 'rows start folded');
await row('acme/picture').locator(':scope > summary').click();
const body = await row('acme/picture').locator('.tool-body').textContent();
ok(await row('acme/picture').evaluate((d) => d.open), 'a tap opens the row');
ok(/Input/.test(body) && /"prompt": "a cat"/.test(body) && /"id": "acme\/picture"/.test(body), 'it shows the input: ' + body.slice(0, 120));
ok(/Result/.test(body) && /cost_usd/.test(body) && body.includes(PICTURE), 'and the answer');
ok(/Cost\$0\.0123/.test(body.replace(/\s+/g, '')), 'and the exact cost: ' + (await row('acme/picture').locator('.tool-charged').textContent()));
keep('thinking-open');
await row('acme/broken').locator(':scope > summary').click();
ok(/The tool broke upstream/.test(await row('acme/broken').locator('.tool-body').textContent()), 'a failed row says why');
await row('bitcoin price').locator(':scope > summary').click();
ok(/Bitcoin is about \$70,000/.test(await row('bitcoin price').locator('.tool-body').textContent()) && /Free/.test(await row('bitcoin price').locator('.tool-charged').textContent()), 'the search row shows its results and that it was free');
const run = fold().locator('.run');
ok(await run.count() === 1 && /42/.test(await run.locator('.run-out').textContent()), 'the code run is in the fold with its output');

// The picture stays in view.
ok(await reply().locator('img.run-img').count() === 1 && await fold().locator('img').count() === 0, 'the picture the tool made is outside the fold');
ok(await reply().locator('.media-save').count() === 1, 'with its Save button');

// After a restart: the same, folded.
({ page } = await launch(false));
await page.waitForSelector('details.thinking', { timeout: 20000 });
ok(!(await fold().evaluate((d) => d.open)), 'reopened, the fold starts folded');
ok((await fold().locator(':scope > summary').textContent()).trim() === label, 'with the same title');
await fold().locator(':scope > summary').click();
const again = await fold().evaluate((d) => [...d.querySelectorAll('.think, .step-text, .step > .tool-call > summary, .run-head')]
  .map((n) => n.textContent.trim().replace(/\s+/g, ' ')));
ok(JSON.stringify(again) === JSON.stringify(order), 'in the same order');
await row('acme/picture').locator(':scope > summary').click();
ok(/"prompt": "a cat"/.test(await row('acme/picture').locator('.tool-body').textContent()), 'and a row still opens to its input');
ok(await reply().locator('img.run-img').count() === 1, 'the picture is still outside');
console.log('\nall thinking-section checks passed');
process.exit(0);
