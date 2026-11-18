// Private sessions (Unlinkable inference), on by default: the account number is made on the phone and only a
// key derived from it is sent; chat requests carry a session secret and no cookie and are paid by
// a throwaway session, not the account; and after the app's data is wiped, signing in with the
// number brings the wallet and the money left in the open sessions back to the millicent. A reply
// the session can't cover (the server holds its worst case first) gets 402: the app tops the
// session up and asks again, on a chat (a pricey model) and on a tool call (an injected 402).
//
// The app talks to a recorder on :8231 that forwards to the dev server on :8230, so the test sees
// exactly what left the phone:
//   OBSCURIFY_API=http://127.0.0.1:8231 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8231 tcp:8231; DB=obscurify_unlinkable node private.mjs
import http from 'node:http';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG, wordsOn } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8230) };
const DB = process.env.DB || 'obscurify_unlinkable';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const seen = [];
let inject402 = false;
const bearer = (r) => (r.headers.authorization || '').replace('Bearer ', '');
// A 402 on `path` for a session, then a top-up of that session, then a 2xx on `path` with it.
const toppedUp = (path, from) => {
  const later = seen.slice(from);
  const i = later.findIndex((r) => r.path.startsWith(path) && r.status === 402 && bearer(r).startsWith('sk-unl1_'));
  if (i < 0) return null;
  const secret = bearer(later[i]);
  const t = later.findIndex((r, j) => j > i && r.path === '/u/sessions/topup' && bearer(r) === secret && r.status === 200);
  const done = later.findIndex((r, j) => j > t && r.path.startsWith(path) && bearer(r) === secret && r.status >= 200 && r.status < 300);
  return t > 0 && done > 0 ? { secret, injected: !!later[i].injected } : null;
};
const recorder = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    const entry = { method: req.method, path: req.url, headers: req.headers, body: body.toString() };
    seen.push(entry);
    // The first tool call on a private session is answered 402, as a hold the session can't cover
    // would be: the app has to top the session up and ask again.
    if (inject402 && req.url.startsWith('/web/tools/') && /^Bearer sk-unl1_/.test(req.headers.authorization || '')) {
      inject402 = false;
      entry.status = 402;
      entry.injected = true;
      res.writeHead(402, { 'content-type': 'application/json' });
      return res.end(JSON.stringify({ error: { message: 'Insufficient funds' } }));
    }
    const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => {
      entry.status = r.statusCode;
      res.writeHead(r.statusCode, r.headers);
      r.pipe(res);
    });
    up.on('error', () => res.destroy());
    up.end(body);
  });
});
await new Promise((r) => recorder.listen(8231, '127.0.0.1', r));
adb('reverse', 'tcp:8231', 'tcp:8231');
const files = `/data/user/0/${PKG}/files`;
const walletMillicents = () => {
  let w;
  try { w = JSON.parse(execFileSync('adb', ['shell', 'cat', `${files}/unlinkable-state.json`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] })).wallet; } catch { return -1; }
  if (!w) return -1;
  return w.vouchers.reduce((s, h) => s + h.voucher.denomination_cents * (h.voucher.dust ? 1 : 1000), 0);
};

try {
  // 1. Sign up.
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Make My Account Words' }).click();
  const number = await wordsOn(page, '.words', 30000);
  ok(/^([a-z]+ ){11}[a-z]+$/.test(number), 'the phone made 12 account words');
  const digits = number;
  const signup = seen.find((r) => r.path === '/create_account');
  ok(signup && /"account_key":"[0-9a-f]{64}"/.test(signup.body) && !signup.body.includes(digits), 'the server got only a key');
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  shot('private-signed-up');

  // Fund the account ($5, as a deposit: there is no payment on dev). It is found by its key hash.
  const key = execFileSync(process.env.SESSIONS_BIN || `${process.env.HOME}/Work/unlinkable-inference-words/target/release/unlinkable-inference`, ['login-key'], { input: digits }).toString().trim();
  const secretFile = process.env.SECRET_FILE || `${process.env.HOME}/Work/obscurify-unlinkable/local/anon_keys/account_secret`;
  const hash = execFileSync('python3', ['-c', 'import hmac,hashlib,sys; print(hmac.new(bytes.fromhex(open(sys.argv[1]).read().strip()), b"obscurify-login-key-v1:"+sys.argv[2].encode(), hashlib.sha256).hexdigest())', secretFile, key]).toString().trim();
  const userId = sql(`SELECT id FROM users WHERE account_number_hash = '${hash}'`);
  ok(/^[0-9a-f-]{36}$/.test(userId), 'the account is stored under the key hash');
  sql(`UPDATE users SET balance = balance + 5 WHERE id = '${userId}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${userId}', 5, 'deposit')`);

  // 2. Chat (the wallet fills on the first tick after sign-in).
  await page.locator('.drawer .bar .icon').click().catch(() => {});
  await agentSwitches(page, false);
  await page.locator('#composer').fill('Reply with exactly the word: pineapple');
  await page.locator('.send').click();
  await page.waitForSelector('.msg.assistant .meta', { timeout: 120000 });
  const answer = await page.locator('.msg.assistant').last().textContent();
  ok(/pineapple/i.test(answer), 'answered: ' + answer.slice(0, 40));
  shot('private-chat');
  const chat = seen.filter((r) => r.path === '/web/chat/completions').pop();
  ok(chat && /^Bearer sk-unl1_/.test(chat.headers.authorization || ''), 'sent with a session secret');
  ok(chat && !chat.headers.cookie, 'sent with no cookie');
  const newChat = async () => { await page.locator('.bar .icon', { hasText: '＋' }).click(); await sleep(500); };
  const pick = async (search, name) => {
    await page.locator('.title.model').click();
    await page.locator('input.search').fill(search);
    await page.locator('.pick', { hasText: name }).filter({ hasNotText: 'Thinking' }).first().locator('.grow').click();
  };
  const ask = async (text, timeout = 240000) => {
    const before = await page.locator('.msg.assistant, .msg.err').count();
    await page.locator('#composer').fill(text);
    await sleep(300);
    await page.locator('.send').click();
    await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), before, { timeout });
    return (await page.locator('.msg.assistant, .msg.err').last().textContent()).trim();
  };

  // 2b. A pricey model: the server holds the reply's worst case (about $1 of output), more than a
  // new session starts with, so it answers 402 until the app has topped the session up.
  await newChat();
  await pick('Claude 4.1 Opus', 'Anthropic: Claude 4.1 Opus');
  let from = seen.length;
  const mango = await ask('Reply with exactly the word: mango');
  ok(/mango/i.test(mango), 'pricey model answered: ' + mango.slice(0, 40));
  const held = toppedUp('/web/chat/completions', from);
  ok(held && !held.injected, 'a real 402 hold on the chat was topped up and asked again');
  shot('private-pricey');

  // 2c. Tools (web_post): the first tool call gets a 402, the app tops up and asks again.
  await newChat();
  await pick('DeepSeek V4 Flash', 'DeepSeek: DeepSeek V4 Flash');
  await agentSwitches(page, true);
  from = seen.length;
  inject402 = true;
  const looked = await ask('Search the web: what is the current population of Reykjavik? One sentence.');
  ok(!inject402, 'a tool call went out on a private session');
  const tool = toppedUp('/web/tools/', from);
  ok(tool && tool.injected, 'a 402 on a tool call was topped up and asked again');
  ok(!/insufficient|kept failing/i.test(looked) && looked.length > 10, 'the lookup answered: ' + looked.slice(0, 60));
  shot('private-tools');

  const usage = sql(`SELECT COUNT(*) FROM transactions WHERE user_id = '${userId}' AND transaction_type NOT IN ('deposit', 'voucher_withdrawal', 'starter_credit')`);
  ok(usage === '0', 'the account was not charged for the chats');
  const secrets = [...new Set(seen.map(bearer).filter((b) => b.startsWith('sk-unl1_')))];
  const hashes = secrets.map((s) => `'${createHash('sha256').update(s).digest('hex')}'`).join(',');
  const open = +sql(`SELECT COALESCE(SUM(CEIL(u.balance * 100000)), 0)::BIGINT FROM unlinkable_sessions s JOIN users u ON u.id = s.user_id WHERE s.closed_at IS NULL AND s.secret_hash IN (${hashes})`);
  ok(secrets.length === 3, `each chat had its own session (${secrets.length} for 3 chats)`);
  ok(open > 0, 'their sessions are still open');
  const had = walletMillicents() + open;

  // 2b. The Account screen on this phone: the balance includes the money held for private chats
  // (still the customer's), each chat shows as spent straight away (its session still open) and is
  // listed; the move into the wallet is not a charge.
  const accountBalance = Number(sql(`SELECT balance FROM users WHERE id = '${userId}'`));
  await page.locator('.bar .icon').first().click();
  await page.getByText('Account', { exact: false }).last().click();
  const expect = '$' + (accountBalance + had / 100000).toFixed(2);
  let shown = '';
  for (let i = 0; i < 30 && shown !== expect; i++) { await sleep(1000); shown = (await page.locator('.section .big').first().textContent()).trim(); }
  ok(shown === expect, `the balance includes the money held for private chats: ${shown} (expected ${expect})`);
  const hist = page.locator('.history');
  await hist.scrollIntoViewIfNeeded();
  await hist.locator('.row.tap').first().waitFor({ timeout: 20000 });
  const today = await hist.locator('.row.tap').first().textContent();
  // A chat that cost under 0.001¢ costs nothing (sessions round in the customer's favour) and isn't listed.
  const state = JSON.parse(execFileSync('adb', ['shell', 'cat', `${files}/unlinkable-state.json`], { encoding: 'utf8' })).wallet.sessions;
  const costly = state.filter((x) => {
    const h = createHash('sha256').update(x.secret).digest('hex');
    return x.funded_millicents > +sql(`SELECT CEIL(u.balance * 100000)::BIGINT FROM unlinkable_sessions s JOIN users u ON u.id = s.user_id WHERE s.secret_hash = '${h}'`);
  }).length;
  ok(costly >= 2 && new RegExp(costly + ' private chats').test(today) && !/prepaid|wallet/i.test(today), `today: ${costly} private chats that cost something, nothing about wallets: ` + today);
  await hist.locator('.row.tap').first().click();
  await hist.locator('.history-head').waitFor();
  ok(await hist.getByText('Private chat', { exact: true }).count() === costly, 'each listed as "Private chat"');
  ok(/Private chat.*−\$/.test(await hist.textContent()) && !/prepaid|wallet/i.test(await hist.textContent()), 'with what it cost, as a debit; no move to a wallet');
  shot('private-history');

  // 3. Wipe the app (a new phone), sign in with the number.
  ({ page } = await launch(true));
  await page.locator('.login input').fill(number);
  await page.getByRole('button', { name: 'Sign In' }).click();
  await page.waitForSelector('#composer', { timeout: 60000 });
  const login = seen.filter((r) => r.path === '/login').pop();
  ok(login && login.body.includes('account_key') && !login.body.includes(digits), 'signed in with only a key');
  let got = -1;
  for (let i = 0; i < 90 && got !== had; i++) { await sleep(1000); got = walletMillicents(); }
  ok(got === had, `every millicent came back (${got} of ${had})`);
  ok(sql(`SELECT COUNT(*) FROM unlinkable_sessions WHERE closed_at IS NULL AND secret_hash IN (${hashes})`) === '0', 'the stranded sessions were closed');
  shot('private-restored');
  console.log('all private-session checks passed');
} finally {
  recorder.close();
}
process.exit(0);
