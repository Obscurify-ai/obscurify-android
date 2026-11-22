// A phone that just signed in to an account with chats: the list says it's loading them (never that
// there are none), and shows them as they come, right after signing in (src/sync.rs).
//   OBSCURIFY_API=http://127.0.0.1:8233 DEVTOOLS=1 APP_ID=ai.obscurify.loadingchats ./build.sh x86_64 && adb install -r target/obscurify.apk
//   PKG=ai.obscurify.loadingchats UPSTREAM_PORT=8097 node loading-chats.mjs
import http from 'node:http';
import { chromium } from 'playwright';
import { launch, adb, ok, shot, sleep, wordsOn } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
let hold = null; // a promise: sync answers wait for it
const recorder = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', async () => {
    if (req.url.startsWith('/sync/') && hold) await hold;
    const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
    up.on('error', () => res.destroy());
    up.end(Buffer.concat(chunks));
  });
});
await new Promise((r) => recorder.listen(8233, '127.0.0.1', r));
adb('reverse', 'tcp:8233', 'tcp:8233');
const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true });
const drawer = (page) => page.locator('.icon', { hasText: '☰' }).click();
try {
  // An account, made on the phone, with no chats.
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Create Account' }).click();
  const code = await wordsOn(page);
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  await drawer(page);
  await page.locator('.drawer').getByText('No chats yet').waitFor({ timeout: 10000 });
  ok(true, 'a new account has no chats, and says so');

  // Chats made on the website.
  const web = await (await browser.newContext()).newPage();
  await web.goto(`http://${UPSTREAM.host}:${UPSTREAM.port}/`);
  await web.waitForFunction(() => window.PrivateSessions && window.ChatSync && typeof chatStore !== 'undefined');
  await web.evaluate(async (n) => {
    localStorage.setItem('obscurify_private_setup_later', '1');
    localStorage.setItem('obscurify_private_off', '1');
    await PrivateSessions.keys(n);
  }, code);
  await web.reload();
  await web.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined');
  await web.evaluate(async () => {
    await chatStore.ready;
    await chatStore.save(Array.from({ length: 12 }, (_, i) => ({ id: 'conv-load-' + i, title: 'Chat ' + i, model: 'test/model', createdAt: Date.now() - i * 1e6, updatedAt: Date.now() - i * 1e6,
      messages: [{ role: 'user', content: 'hello ' + i }, { role: 'assistant', content: 'hi', display: 'hi' }] })));
    await ChatSync.syncNow();
    await ChatSync.syncNow();
  });

  // A fresh install signs in, over a connection that takes its time.
  let go;
  hold = new Promise((r) => { go = r; });
  ({ page } = await launch(true));
  await page.locator('.login input').fill(code);
  await page.locator('.login button.go').click();
  await page.waitForSelector('#composer', { timeout: 60000 });
  await drawer(page);
  await page.locator('.drawer').getByText('Loading chats…').waitFor({ timeout: 10000 });
  ok(!(await page.locator('.drawer').getByText('No chats yet').count()), 'signed in, the list says its chats are loading');
  shot('loading-chats');
  await sleep(3000);
  ok(await page.locator('.drawer').getByText('Loading chats…').count() === 1, '  for as long as they take');
  const at = Date.now();
  hold = null;
  go();
  await page.locator('.drawer .item').nth(11).waitFor({ timeout: 10000 });
  ok(true, `then shows them, without waiting for the next round: all 12 in ${Date.now() - at} ms`);
  ok(!(await page.locator('.drawer').getByText(/Loading chats|No chats yet/).count()), '  and nothing else');
  shot('loading-chats-done');
  console.log('\nall checks passed');
} finally {
  await browser.close();
  recorder.close();
  try { adb('reverse', '--remove', 'tcp:8233'); } catch {}
}
process.exit(0);
