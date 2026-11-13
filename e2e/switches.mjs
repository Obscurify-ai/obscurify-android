// The Tools and Code switches end to end, with a real model, as on the web (tools-switch.mjs there).
// Both on by default: the model calls web search and the marketplace directly and runs code only
// when it needs to. Code off: tools only, no run_code. Tools off: run_code only, and code can't
// reach the tools either. Both off: plain chat. The choice survives a restart. Spends a cent or two.
// Needs a server with separate switches (/code_mode has `prompts`) and flaky-proxy.mjs, which logs
// what the app sends:
//
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8093 tcp:8093 && node flaky-proxy.mjs &
//   ACCOUNT=<funded number> node switches.mjs
import { launch, ok, shot, sleep, flip } from './lib.mjs';

const ACCOUNT = process.env.ACCOUNT;
const MODEL_SEARCH = process.env.MODEL_SEARCH || 'DeepSeek V4 Flash';
const MODEL_NAME = process.env.MODEL_NAME || 'DeepSeek: DeepSeek V4 Flash';
if (!ACCOUNT) throw new Error('ACCOUNT=<16-digit account number> is required');

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const reset = () => ctl('__set?after=0&fail=0');
const log = async () => JSON.parse(await ctl('__log'));
const toolLog = async () => JSON.parse(await ctl('__tools'));
const offered = (r) => (r.tools || []).map((t) => t.function.name).sort().join(',');
const system = (r) => (r.messages[0].role === 'system' ? String(r.messages[0].content) : '');
// The tools the model called in this reply, in order, once each.
const called = (reqs) => reqs.flatMap((r) => r.messages).filter((m) => m.tool_calls)
  .flatMap((m) => m.tool_calls.map((c) => c.function.name)).filter((v, i, a) => a.indexOf(v) === i);

let { page } = await launch(true);
await reset();
await page.waitForSelector('.login input');
await page.locator('.login input').fill(ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.code-switch', { state: 'attached', timeout: 30000 });
ok(true, 'signed in, and the Code switch is there');

const state = async () => ({ tools: await page.locator('.tools-switch').getAttribute('aria-checked'), code: await page.locator('.code-switch').getAttribute('aria-checked') });
let s = await state();
ok(s.tools === 'true' && s.code === 'true', 'Tools and Code are both on by default: ' + JSON.stringify(s));
{
  const [line, box, plus, send] = await Promise.all(['.composer .line', '#composer', '.plus', '.send'].map((q) => page.locator(q).boundingBox()));
  ok(plus.x >= box.x + box.width - 1 && send.x >= plus.x + plus.width - 1 && plus.y >= line.y - 1 && box.width >= 200,
    'one box: the message, then + and Send at the right: ' + JSON.stringify({ box, plus, send }));
  await page.locator('.plus').click();
  ok((await page.locator('.plus-menu .item .label').allTextContents()).join('|') === 'Attach files|Tools|Code', '+ holds Attach files, Tools and Code');
  await page.locator('.code-switch').click();
  ok(await page.locator('.plus-menu.open').count() === 1 && (await state()).code === 'false', 'flipping Code leaves the menu open to show it');
  await page.locator('.code-switch').click();
}
shot('switches-both-on');
await page.locator('.menu-veil').click({ position: { x: 10, y: 10 } });

await page.locator('.title.model').click();
await page.locator('input.search').fill(MODEL_SEARCH);
await page.locator('.pick', { hasText: MODEL_NAME }).first().locator('.grow').click();
ok(true, 'model: ' + (await page.locator('.title.model').textContent()));

async function ask(text, timeout = 240000) {
  await reset();
  const before = await page.locator('.msg.assistant').count();
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant').length > n && !document.querySelector('.send.stop'), before, { timeout });
  const reply = page.locator('.msg.assistant').last();
  return { reply, text: (await reply.textContent()).trim(), reqs: await log(), tools: await toolLog() };
}
const newChat = async () => { await page.locator('.bar .icon', { hasText: '＋' }).click(); await sleep(500); };

// 1. Both on: a lookup is a direct web_search, not code.
let r = await ask('Search the web: what is the current population of Reykjavik? One sentence.');
ok(offered(r.reqs[0]) === 'call_tool,find_tools,inspect_tool,run_code,web_search', 'both on: the model gets run_code and the direct tools: ' + offered(r.reqs[0]));
ok(/Call tools directly whenever/.test(system(r.reqs[0])), 'both on: the system prompt says to call tools directly');
ok(called(r.reqs).join(',') === 'web_search', 'both on: a web lookup calls web_search directly, without code: ' + called(r.reqs).join(','));
ok(r.tools.some((t) => t.path.endsWith('/tools/web_search')), 'the app ran it against the tools endpoint: ' + r.tools.map((t) => t.path).join(', '));
ok(await r.reply.locator('.thinking .step > .tool-call .tool-name', { hasText: 'web_search' }).count() > 0 && await r.reply.locator('.run').count() === 0,
  'the reply shows the search as a tool call in the thinking section, and no code run');
ok(/\d/.test(r.text), 'a real answer: ' + r.text.slice(0, 120));
const toolMsg = r.reqs.at(-1).messages.find((m) => m.role === 'tool');
ok(toolMsg && /results|answer/.test(toolMsg.content), 'the search came back to the model as JSON: ' + String(toolMsg && toolMsg.content).slice(0, 80));
shot('switches-direct');

// 2. Code off: tools only.
await newChat();
await flip(page, '.code-switch');
s = await state();
ok(s.tools === 'true' && s.code === 'false', 'Code switches off on its own: ' + JSON.stringify(s));
r = await ask('Search the web: who won the most recent Tour de France? Name only.');
ok(offered(r.reqs[0]) === 'call_tool,find_tools,inspect_tool,web_search', 'code off: no run_code, the direct tools only: ' + offered(r.reqs[0]));
ok(system(r.reqs[0]) && !system(r.reqs[0]).includes('run_code'), 'code off: the prompt never mentions run_code');
ok(called(r.reqs).join(',') === 'web_search', 'code off: web_search was called: ' + called(r.reqs).join(','));
ok(r.text.length > 2 && await r.reply.locator('.run').count() === 0, 'code off: a real answer, no code run: ' + r.text.slice(0, 120));

// 3. Tools off, Code on: code with no tools, and code can't reach them.
await newChat();
await flip(page, '.code-switch');
await flip(page, '.tools-switch');
s = await state();
ok(s.tools === 'false' && s.code === 'true', 'Tools off, Code on: ' + JSON.stringify(s));
r = await ask('Use run_code with Python to run exactly this, then tell me what it printed:\n\nprint(6 * 7)\ntry:\n    print(await tools.web_search("weather"))\nexcept Exception as e:\n    print("refused:", e)');
ok(offered(r.reqs[0]) === 'run_code', 'tools off: only run_code: ' + offered(r.reqs[0]));
ok(/no tools are available to code/i.test(system(r.reqs[0])), 'tools off: the prompt says code has no tools');
ok(await r.reply.locator('.run.ok').count() > 0 && /42/.test(r.text), 'tools off: the code really runs: ' + r.text.slice(0, 160));
ok(r.tools.length === 0, 'tools off: a tool call from inside the code never left the phone: ' + JSON.stringify(r.tools.map((t) => t.path)));
ok(/not available/i.test(await r.reply.locator('.run-out').first().textContent()), 'the sandbox was told no: ' + (await r.reply.locator('.run-out').first().textContent()).slice(0, 160));

// 4. Both off in a chat that ran code: plain chat, and it keeps the chat.
await flip(page, '.code-switch');
s = await state();
ok(s.tools === 'false' && s.code === 'false', 'both off: ' + JSON.stringify(s));
r = await ask('Now add 8 to the first number it printed. Reply with just the number.');
ok(r.reqs.length === 1 && !r.reqs[0].tools && !system(r.reqs[0]), 'both off: plain chat, no tools and no system prompt');
ok(/50/.test(r.text), 'both off: the follow-up keeps the chat: ' + r.text.slice(0, 80));

// 5. A restart keeps both off; then both back on.
({ page } = await launch(false));
await page.waitForSelector('.code-switch', { state: 'attached', timeout: 30000 });
s = await state();
ok(s.tools === 'false' && s.code === 'false', 'off survives a restart: ' + JSON.stringify(s));
await flip(page, '.tools-switch');
await flip(page, '.code-switch');
s = await state();
ok(s.tools === 'true' && s.code === 'true', 'both back on: ' + JSON.stringify(s));
console.log('\nall Tools and Code switch checks passed');
process.exit(0);
