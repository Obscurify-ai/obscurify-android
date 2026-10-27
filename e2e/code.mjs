// Code Mode on the phone, end to end: the model runs Python in the sandbox WebView, draws a chart,
// calls a paid tool billed to the balance, and builds a page that opens full screen. Then, from
// inside the sandbox WebView itself: no app bridges, no network, no cookies, no way to navigate
// away. Needs a funded account on the server the app was built for:
//
//   OBSCURIFY_API=https://<dev server> DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   ACCOUNT=<16 digits> MODEL=anthropic/claude-haiku-5.5 node code.mjs
import { launch, adb, ok, shot, sleep, PKG } from './lib.mjs';

const ACCOUNT = process.env.ACCOUNT;
const MODEL_SEARCH = process.env.MODEL_SEARCH || 'haiku 5.5';
if (!ACCOUNT) throw new Error('ACCOUNT=<16-digit account number> is required');

let { page } = await launch(true);
await page.waitForSelector('.login input');
await page.locator('.login input').fill(ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });
ok(true, 'signed in');

await page.locator('.title.model').click();
await page.locator('input.search').fill(MODEL_SEARCH);
await page.locator('.pick .grow').first().click();
ok(true, 'model: ' + (await page.locator('.title.model').textContent()));
ok(await page.locator('.chip', { hasText: 'Web search' }).count() === 0, 'no Web search switch: code has web search as a tool');

async function ask(text, timeout = 240000) {
  const before = await page.locator('.msg.assistant').count();
  await page.locator('#composer').fill(text);
  await page.locator('.send').click();
  await page.waitForFunction(n => document.querySelectorAll('.msg.assistant').length > n && !document.querySelector('.send.stop'), before, { timeout });
  return page.locator('.msg.assistant').last();
}

// 1. Python with a chart.
let reply = await ask('Use Python to compute the squares of 1 to 10, plot them as a bar chart, and tell me their sum.');
ok(await reply.locator('.run.ok').count() > 0, 'code ran in the sandbox: ' + (await reply.locator('.run-head').first().textContent()));
ok(await reply.locator('img.run-img').count() > 0, 'chart shown');
ok(/385/.test(await reply.textContent()), 'answered with the sum (385)');
shot('code-chart');

// 2. A paid tool, billed.
reply = await ask('Use a paid tool to get the 2-day weather forecast for Miami. Answer in one line.');
const toolNames = await reply.locator('.run-tools .tool-name').allTextContents();
ok(toolNames.some(n => n.trim() === 'call'), 'paid tool called: ' + toolNames.map(s => s.trim()).join(', '));
ok(await reply.locator('.run-cost').count() > 0, 'its cost shown: ' + (await reply.locator('.run-cost').first().textContent()));
shot('code-paid');

// 3. A page, full screen in a sandbox WebView of its own.
reply = await ask('Build a tiny HTML page with a big heading "Hello from the sandbox" and a button that counts clicks. Show it with display_html.');
const pageButtons = reply.locator('.page-btn');
ok(await pageButtons.count() > 0, 'page button: ' + (await pageButtons.first().textContent()));
await pageButtons.first().click();
await sleep(3000);
const ui = adb('exec-out', 'uiautomator', 'dump', '/dev/tty');
ok(/text="Close"/i.test(ui), 'page opened full screen with a Close button');
shot('code-page');

// The app process has several WebViews now; inspect them over the DevTools socket, with the
// raw protocol (Playwright can't attach to a WebView browser).
const pid = adb('shell', 'pidof', PKG);
adb('forward', 'tcp:9339', `localabstract:webview_devtools_remote_${pid}`);
const targets = async () => (await (await fetch('http://127.0.0.1:9339/json')).json()).filter(t => t.type === 'page');
async function evaluate(target, expression) {
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  const reply = new Promise(res => { ws.onmessage = m => { const d = JSON.parse(m.data); if (d.id === 1) res(d); }; });
  ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
  const d = await reply;
  ws.close();
  return d.result && d.result.result ? d.result.result.value : undefined;
}
let list = await targets();
const artifact = list.find(t => t.url.includes('/sandbox/artifact.html'));
ok(artifact && /Hello from the sandbox/.test(await evaluate(artifact, 'document.body.innerText')), 'the page rendered in its own WebView');
adb('shell', 'input', 'keyevent', '4');
await sleep(1500);
ok(!/text="Close"/i.test(adb('exec-out', 'uiautomator', 'dump', '/dev/tty')), 'Back closes it');

// 4. Inside the sandbox WebView: nothing to escape with.
list = await targets();
const runner = list.find(t => t.url.includes('/sandbox/runner.html'));
ok(runner, 'sandbox WebView found: ' + (runner && runner.url));
const probe = await evaluate(runner, `(async () => {
  // window.android may exist: newer WebViews put their own android.webview API there. Ours has
  // open/copy/sandbox*; wry's is window.ipc.
  const bridge = window.android || {};
  const r = { android: ['open', 'copy', 'sandboxStart', 'sandboxPost', 'openPage'].filter(k => k in bridge).join(',') || 'none', ipc: typeof window.ipc, origin: String(self.origin) };
  try { r.cookie = document.cookie; } catch (e) { r.cookie = 'blocked'; }
  try { r.storage = String(localStorage.length); } catch (e) { r.storage = 'blocked'; }
  for (const [name, url] of [['internet', 'https://example.com/'], ['app api', location.origin + '/get_user_balance']]) {
    try { await fetch(url); r[name] = 'allowed'; } catch (e) { r[name] = 'blocked'; }
  }
  return r;
})()`);
ok(probe.android === 'none' && probe.ipc === 'undefined', 'no app bridges in the sandbox: ' + JSON.stringify(probe));
ok(probe.origin === 'null' && probe.cookie === 'blocked' && probe.storage === 'blocked', 'opaque origin, no cookies or storage');
ok(probe.internet === 'blocked' && probe['app api'] === 'blocked', 'no network');
await evaluate(runner, "location.href = 'https://example.com/?leak=1'; 1").catch(() => {});
await sleep(1500);
ok((await targets()).every(t => !t.url.startsWith('https://example.com')), 'it cannot navigate away');
adb('forward', '--remove', 'tcp:9339');

// 5. After a restart the chat comes back with its code, chart and page.
({ page } = await launch(false));
await page.waitForSelector('.msg.assistant', { timeout: 20000 });
ok(await page.locator('.run').count() >= 3 && await page.locator('img.run-img').count() > 0 && await page.locator('.page-btn').count() > 0, 'runs, charts and pages kept after a restart');
shot('code-restart');
console.log('\nall Code Mode checks passed');
process.exit(0);
