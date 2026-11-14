// Private sessions (Chaumian sessions), on by default: the account number is made on the phone and only a
// key derived from it is sent; chat requests carry a session secret and no cookie and are paid by
// a throwaway session, not the account; and after the app's data is wiped, signing in with the
// number brings the wallet and the money left in the open session back to the millicent.
//
// The app talks to a recorder on :8231 that forwards to the dev server on :8230, so the test sees
// exactly what left the phone:
//   OBSCURIFY_API=http://127.0.0.1:8231 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8231 tcp:8231; DB=obscurify_chaumian node private.mjs
import http from 'node:http';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8230) };
const DB = process.env.DB || 'obscurify_chaumian';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const seen = [];
const recorder = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    seen.push({ method: req.method, path: req.url, headers: req.headers, body: body.toString() });
    const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => {
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
  try { w = JSON.parse(execFileSync('adb', ['shell', 'cat', `${files}/chaumian-state.json`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] })).wallet; } catch { return -1; }
  if (!w) return -1;
  return w.vouchers.reduce((s, h) => s + h.voucher.denomination_cents * (h.voucher.dust ? 1 : 1000), 0);
};

try {
  // 1. Sign up.
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Generate Account Number' }).click();
  const number = (await page.locator('.number').textContent({ timeout: 30000 })).trim();
  ok(/^\d{4} \d{4} \d{4} \d{4}$/.test(number), 'the phone made a number');
  const digits = number.replace(/\D/g, '');
  const signup = seen.find((r) => r.path === '/create_account');
  ok(signup && /"account_key":"[0-9a-f]{64}"/.test(signup.body) && !signup.body.includes(digits), 'the server got only a key');
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  shot('private-signed-up');

  // Fund the account ($5, as a deposit: there is no payment on dev). It is found by its key hash.
  const key = execFileSync(process.env.SESSIONS_BIN || `${process.env.HOME}/Work/chaumian-sessions/target/release/chaumian-sessions`, ['login-key'], { input: digits }).toString().trim();
  const secretFile = process.env.SECRET_FILE || `${process.env.HOME}/Work/obscurify-chaumian/local/anon_keys/account_secret`;
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
  ok(chat && /^Bearer sk-chs1_/.test(chat.headers.authorization || ''), 'sent with a session secret');
  ok(chat && !chat.headers.cookie, 'sent with no cookie');
  const usage = sql(`SELECT COUNT(*) FROM transactions WHERE user_id = '${userId}' AND transaction_type NOT IN ('deposit', 'voucher_withdrawal', 'starter_credit')`);
  ok(usage === '0', 'the account was not charged for the chat');
  const secretHash = createHash('sha256').update(chat.headers.authorization.replace('Bearer ', '')).digest('hex');
  const open = +sql(`SELECT COALESCE(SUM(CEIL(u.balance * 100000)), 0)::BIGINT FROM chaumian_sessions s JOIN users u ON u.id = s.user_id WHERE s.closed_at IS NULL AND s.secret_hash = '${secretHash}'`);
  ok(open > 0, 'its session is still open');
  const had = walletMillicents() + open;

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
  ok(sql(`SELECT COUNT(*) FROM chaumian_sessions WHERE closed_at IS NULL AND secret_hash = '${secretHash}'`) === '0', 'the stranded session was closed');
  shot('private-restored');
  console.log('all private-session checks passed');
} finally {
  recorder.close();
}
process.exit(0);
