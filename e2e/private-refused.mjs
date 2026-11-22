// A chat that can't go privately is not sent (src/private.rs, secret): nothing goes out with the
// account's session and the app says why. With money in the account, or the server's private
// session endpoints back, the next message goes privately. The app talks to a recorder here that
// forwards to the dev server:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   UPSTREAM_PORT=8097 DB=<dev database> node e2e/private-refused.mjs
import http from 'node:http';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, wordsOn } from './lib.mjs';

const PORT = +(process.env.UPSTREAM_PORT || 8097);
const DB = process.env.DB || 'obscurify_sync';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q], { encoding: 'utf8' }).trim();
let chats = [];
let noSessions = false; // the private session endpoints answer 503
const recorder = http.createServer((req, res) => {
  if (/^\/web\/(chat|tools|replies)/.test(req.url)) chats.push({ path: req.url, auth: req.headers.authorization || '', cookie: req.headers.cookie || '' });
  if (noSessions && req.url.startsWith('/u/')) { req.resume(); res.writeHead(503); res.end(); return; }
  const up = http.request({ host: '127.0.0.1', port: PORT, method: req.method, path: req.url, headers: { ...req.headers, host: `127.0.0.1:${PORT}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on('error', () => res.destroy());
  req.pipe(up);
});
await new Promise((r) => recorder.listen(0, '127.0.0.1', r));
adb('reverse', 'tcp:8232', `tcp:${recorder.address().port}`);

let page;
let id;
// A new account on a new install, with nothing in it: whatever the sign-up gave is taken away.
const signUp = async () => {
  ({ page } = await launch(true));
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Create Account' }).click();
  await wordsOn(page, '.number', 40000);
  id = sql('SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1');
  setBalance(0);
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  await page.locator('.title.model').click();
  await page.locator('input.search').fill('DeepSeek V4 Flash');
  await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
};
const setBalance = (b) => sql(`DELETE FROM transactions WHERE user_id = '${id}'; UPDATE users SET balance = ${b} WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) SELECT '${id}', ${b}, 'deposit' WHERE ${b} > 0`);
const invoices = () => sql(`SELECT count(*) FROM invoices WHERE user_id = '${id}'`);
const last = () => page.locator('.msg.assistant, .msg.err').last();
const ask = async (text) => {
  const n = await page.locator('.msg.assistant, .msg.err').count();
  chats = [];
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), n, { timeout: 120000 });
  return (await last().innerText()).trim();
};
const fresh = async () => { await page.locator('.bar .icon', { hasText: '＋' }).click().catch(() => {}); await sleep(700); };
const privately = () => chats.length > 0 && chats.every((c) => /^Bearer sk-unl1_/.test(c.auth) && !c.cookie);

let said = '';
for (const tools of [false, true]) {
  await signUp();
  await agentSwitches(page, tools);
  for (const word of ['mango', 'mango again']) {
    said = await ask(`Reply with exactly: ${word}`);
    ok(/at least 10¢ in your account, so nothing was sent/.test(said), `empty account (tools ${tools}): refused, saying why`);
    ok(chats.length === 0 && invoices() === '0', '  nothing left the phone, nothing was billed to the account');
  }
  if (!tools) shot('private-no-funds');
  setBalance(5);
  await fresh();
  said = await ask('Reply with exactly the word: papaya');
  ok(/papaya/i.test(said) && privately(), 'money added: the next message goes privately at once');
}

// The private session endpoints don't answer when the app starts.
noSessions = true;
page = (await launch(false)).page;
await page.waitForSelector('#composer');
said = await ask('Reply with exactly: cherry');
ok(/could not be started, so nothing was sent/.test(said) && chats.length === 0, `private sessions can't start: refused, nothing left the phone (${said.slice(0, 60)})`);
shot('private-not-started');
noSessions = false;
await fresh();
said = await ask('Reply with exactly the word: lychee');
ok(/lychee/i.test(said) && privately(), 'reachable again: the next message goes privately, without a restart');
ok(invoices() === '0', 'in all, nothing was billed to the account');
console.log('all ok');
process.exit(0);
