// A chat that needs money is funded at once (src/private.rs, the wallet library's default): the
// wallet asks the account and the message goes seconds later, privately, with no wait. An account
// with 30¢ (20¢ of it taken into the wallet when the app opened: one fixed size at a time under $1)
// gets $10 more on its balance elsewhere, then asks a pricey model whose reply needs more than the
// wallet holds. The wallet takes $5 from the account, the chat never says it is waiting for the
// wallet, the message is answered within a minute or so, every chat request carries a session
// secret, and nothing is billed to the account by name.
// The app talks to a recorder here that forwards to the dev server:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   UPSTREAM_PORT=8095 DB=<dev database> [PRICEY_MODEL=<name>] node e2e/private-timing.mjs
// Takes about two minutes. Spends a few cents of real model usage.
import http from 'node:http';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, wordsOn } from './lib.mjs';

const PORT = +(process.env.UPSTREAM_PORT || 8095);
const DB = process.env.DB || 'obscurify_timing';
const PRICEY = process.env.PRICEY_MODEL || 'GPT 5.5';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q], { encoding: 'utf8' }).trim();
const chats = [];
const recorder = http.createServer((req, res) => {
  if (/^\/web\/(chat|tools|replies)/.test(req.url) && req.method === 'POST') {
    chats.push({ session: /sk-unl1_/.test(req.headers.authorization || ''), cookie: !!req.headers.cookie });
  }
  const up = http.request({ host: '127.0.0.1', port: PORT, method: req.method, path: req.url, headers: { ...req.headers, host: `127.0.0.1:${PORT}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on('error', () => res.destroy());
  req.pipe(up);
});
await new Promise((r) => recorder.listen(0, '127.0.0.1', r));
adb('reverse', 'tcp:8232', `tcp:${recorder.address().port}`);

let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
await wordsOn(page, '.number', 40000);
const id = sql('SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1');
const balance = () => Number(sql(`SELECT balance FROM users WHERE id = '${id}'`));
sql(`DELETE FROM transactions WHERE user_id = '${id}'; UPDATE users SET balance = 0.30, pro_access_until = NULL WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 0.30, 'deposit')`);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
// The app opening (a moment of the account's own) fills the wallet.
({ page } = await launch(false));
await page.waitForSelector('#composer');
for (let i = 0; i < 60 && balance() !== 0.1; i++) await sleep(1000);
ok(balance() === 0.1, 'opening the app moved 20¢ into the wallet (one fixed size)');
await agentSwitches(page, false);
// Money arrives some other way (another device's payment): this phone doesn't know.
sql(`UPDATE users SET balance = balance + 10 WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 10, 'deposit')`);
await sleep(20000);
await page.locator('.title.model').click();
await sleep(500);
await page.locator('input.search').fill(PRICEY);
await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
await sleep(500);

const n = await page.locator('.msg.assistant, .msg.err').count();
const started = Date.now();
await page.locator('#composer').fill('Reply with exactly the word: mango');
await sleep(300);
await page.locator('.send').click();
let noticed = false;
for (let i = 0; i < 360; i++) {
  await sleep(500);
  noticed ||= (await page.locator('#private-wait').count()) > 0;
  if (await page.evaluate((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), n)) break;
}
const waited = Math.round((Date.now() - started) / 1000);
const reply = (await page.locator('.msg.assistant, .msg.err').last().innerText()).trim();
shot('private-funded');
ok(/mango/i.test(reply) && waited < 90, `the message went at once and the model answered (${waited} s: ${reply.slice(0, 60)})`);
ok(!noticed, 'the chat never said it was waiting for the wallet');
ok(balance() === 5.1, `the account paid the wallet $5 (a fixed size), not the chat (${balance()})`);
ok(sql(`SELECT count(*) FROM invoices WHERE user_id = '${id}'`) === '0', 'nothing was billed to the account by name');
ok(chats.length > 0 && chats.every((c) => c.session && !c.cookie), `the chat went privately: a session secret, no cookie (${chats.length} requests)`);
console.log('all ok');
process.exit(0);
