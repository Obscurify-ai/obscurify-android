// A plan bought in the app opens its models in the very next private chat: buying it is a moment of
// the account's own, so the wallet asks at once for what the new plan gives (src/private.rs
// `money_added`), instead of learning it at its next contact. A new account on the standard plan
// list (Pay As You Go, frontier models locked) buys Casual from its balance on the Account screen,
// goes straight back to the chat and asks a frontier model: it answers, privately.
// The app talks to a recorder here that forwards to the dev server:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   UPSTREAM_PORT=<dev server port> DB=<dev database> [FRONTIER=<a frontier model>] node e2e/plan-bought.mjs
// Spends a few cents of real model usage.
import { execFileSync } from 'node:child_process';
import http from 'node:http';
import { launch, adb, ok, sleep, agentSwitches, wordsOn } from './lib.mjs';
const DB = process.env.DB || 'obscurify';
const PORT = +(process.env.UPSTREAM_PORT || 8099);
const FRONTIER = process.env.FRONTIER || 'Claude Opus 5.5';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q], { encoding: 'utf8' }).trim();
const chats = [];
const rec = http.createServer((req, res) => {
  if (/^\/web\/(chat|replies)/.test(req.url) && req.method === 'POST') chats.push({ session: /sk-unl1_/.test(req.headers.authorization || ''), cookie: !!req.headers.cookie });
  const up = http.request({ host: '127.0.0.1', port: PORT, method: req.method, path: req.url, headers: { ...req.headers, host: `127.0.0.1:${PORT}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on('error', () => res.destroy()); req.pipe(up);
});
await new Promise((r) => rec.listen(0, '127.0.0.1', r));
adb('reverse', 'tcp:8232', `tcp:${rec.address().port}`);
let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
await wordsOn(page, '.number', 40000);
const id = sql('SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1');
sql(`DELETE FROM transactions WHERE user_id = '${id}'; UPDATE users SET balance = 100, pro_access_until = NULL WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 100, 'deposit')`);
sql(`UPDATE users SET catalog_id = 'standard', plan_id = (SELECT plan_id FROM catalog_plans WHERE catalog_id = 'standard' AND level = 'pay_as_you_go') WHERE id = '${id}'`);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
({ page } = await launch(false));
await page.waitForSelector('#composer');
// The app opening asked the account which passes it gets: none for frontier models yet.
await sleep(5000);
await agentSwitches(page, false);
await sleep(3000);
await page.locator('.bar .icon').first().click();
await page.locator('.foot button', { hasText: 'Account' }).click();
const row = page.locator('.section .row', { hasText: 'Casual ·' });
await row.waitFor({ timeout: 20000 });
await row.getByRole('button', { name: /Upgrade|Switch/ }).click();
await page.getByRole('button', { name: /^Pay \$/ }).click();
for (let i = 0; i < 40 && sql(`SELECT account_tier FROM users WHERE id='${id}'`) !== 'casual'; i++) await sleep(500);
await page.waitForSelector('.section .row .ok:has-text("Current")', { timeout: 20000 });
const bought = Date.now();
await page.locator('.bar .icon').first().click();
await page.waitForSelector('#composer');
await sleep(2500);
await page.locator('.title.model').click();
await sleep(800);
await page.locator('input.search').fill(FRONTIER);
await sleep(1000);
await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
await sleep(500);
// No restart, no waiting for the wallet's next contact.
const n = await page.locator('.msg.assistant, .msg.err').count();
await page.locator('#composer').fill('Reply with exactly the word: kiwi');
await sleep(300);
await page.locator('.send').click();
await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), n, { timeout: 120000 });
const said = (await page.locator('.msg.assistant, .msg.err').last().innerText()).trim();
ok(/kiwi/i.test(said), `bought Casual in the app, no restart: a frontier model answers ${Math.round((Date.now() - bought) / 1000)} s later (${said.slice(0, 50).replace(/\s+/g, ' ')})`);
ok(chats.length && chats.every((c) => c.session && !c.cookie), 'privately');
console.log('all ok');
process.exit(0);
