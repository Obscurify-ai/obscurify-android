// A chat the private wallet can't pay waits for it, and the account is never asked because of it
// (src/private.rs `hold`): a request with the account's key a second before or after a private
// session's ties the two together by timing. An account with 30¢ (20¢ of it taken into the wallet
// when the app opened) gets $10 more on its balance, then asks a pricey model: the chat says
// "Your private wallet is refilling", the wallet asks the account a random 30 s to 5 min later, and
// the message goes a random 30 s to 5 min after that. No account request within 20 s of a chat's.
// The app talks to a recorder here that forwards to the dev server:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   UPSTREAM_PORT=8095 DB=<dev database> [PRICEY_MODEL=<name>] node e2e/private-wait.mjs
// Takes up to about 12 minutes.
import http from 'node:http';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, wordsOn } from './lib.mjs';

const PORT = +(process.env.UPSTREAM_PORT || 8095);
const DB = process.env.DB || 'obscurify_timing';
const PRICEY = process.env.PRICEY_MODEL || 'GPT 5.5';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q], { encoding: 'utf8' }).trim();
const seen = [];
const recorder = http.createServer((req, res) => {
  const auth = req.headers.authorization || '';
  const path = req.url.split('?')[0];
  const side = /sk-unl1_/.test(auth) || path === '/u/sessions' ? 'chat' : (/sk-obs1_/.test(auth) || req.headers.cookie) ? 'account' : null;
  if (side) seen.push({ at: Date.now(), side, path });
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
const opened = Date.now();
({ page } = await launch(false));
await page.waitForSelector('#composer');
for (let i = 0; i < 60 && balance() !== 0.1; i++) await sleep(1000);
ok(balance() === 0.1, 'opening the app moved 20¢ into the wallet');
await agentSwitches(page, false);
sql(`UPDATE users SET balance = balance + 10 WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 10, 'deposit')`);
await sleep(25000);
await page.locator('.title.model').click();
await sleep(500);
await page.locator('input.search').fill(PRICEY);
await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
await sleep(500);
const n = await page.locator('.msg.assistant, .msg.err').count();
await page.locator('#composer').fill('Reply with exactly the word: mango');
await sleep(300);
await page.locator('.send').click();
await page.waitForSelector('#private-wait', { timeout: 120000 });
const said = (await page.locator('#private-wait').innerText()).trim();
ok(/Your private wallet is refilling\. Your message will be sent in about/.test(said), `the chat says it waits: ${said}`);
ok(balance() === 10.1, 'nothing was taken from the account at that moment');
shot('private-wait');
await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop') && !document.getElementById('private-wait'), n, { timeout: 15 * 60000 });
const reply = (await page.locator('.msg.assistant, .msg.err').last().innerText()).trim();
ok(/mango/i.test(reply), `then the message went and the model answered (${reply.slice(0, 60)})`);
ok(balance() === 5.1 && sql(`SELECT count(*) FROM invoices WHERE user_id = '${id}'`) === '0', 'the account paid the wallet $5 (a fixed size), nothing was billed to it by name');
let closest = Infinity;
for (const a of seen.filter((s) => s.side === 'account' && s.at > opened + 60000)) {
  for (const c of seen.filter((s) => s.side === 'chat')) closest = Math.min(closest, Math.abs(a.at - c.at));
}
ok(closest >= 20000, `no account request within 20 s of a chat's, the app opening aside (closest ${Math.round(closest / 1000)} s)`);
console.log('all ok');
process.exit(0);
