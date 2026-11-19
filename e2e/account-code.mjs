// Account codes on the phone (private.rs, api.rs, main.rs): sign-up makes a 20-character code on the
// phone and sends only a key made from it; it signs back in however it's typed; a typo is caught on
// the phone; an account with a 16-digit number signs in, can't sync, and converts to a code (saved
// first), which turns sync on; the old number stops working.
//
// The app talks to a recorder on :8232 that forwards to the website's dev server, so the test sees
// what left the phone:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8232 tcp:8232
//   UPSTREAM_PORT=8097 node account-code.mjs
import http from 'node:http';
import { randomInt } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, PKG, wordsOn, newCode, CODE } from './lib.mjs';
import { accountKey } from '../../obscurify-cli/lib/account.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const loginKey = accountKey;
const seen = [];
const recorder = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    seen.push({ method: req.method, path: req.url, body: body.toString() });
    const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => {
      res.writeHead(r.statusCode, r.headers);
      r.pipe(res);
    });
    up.on('error', () => res.destroy());
    up.end(body);
  });
});
await new Promise((r) => recorder.listen(8232, '127.0.0.1', r));
adb('reverse', 'tcp:8232', 'tcp:8232');
const files = `/data/user/0/${PKG}/files`;
const cat = (name) => { try { return execFileSync('adb', ['shell', 'cat', `${files}/${name}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }); } catch { return ''; } };
// The code in any form: with dashes, without, in any case, or any group of it.
const leaked = (code) => seen.some((r) => { const b = r.body.toUpperCase(); return b.includes(code.replace(/-/g, '')) || code.split('-').some((g) => b.includes(g)); });
const signInTyping = async (page, text) => {
  await page.locator('.login input').fill(text);
  await page.locator('.login button.go').click();
};
const toAccount = async (page) => {
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#sync-switch, #sync-needs-words').first().waitFor({ timeout: 20000 });
};

try {
  // 1. Sign up: an account code, made on the phone.
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Create Account' }).click();
  await page.locator('.number').waitFor({ timeout: 30000 });
  const words = await wordsOn(page, '.number');
  ok(CODE.test(words), 'sign-up shows an account code: ' + words);
  shot('code-signup');
  const created = seen.find((r) => r.path === '/create_account');
  ok(created && /"account_key":"[0-9a-f]{64}"/.test(created.body) && !leaked(words), '  only a key made from it left the phone');
  ok(cat('account-kind.json').includes('code'), '  the phone knows it signed in with a code');
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  await toAccount(page);
  ok(await page.locator('#sync-on').isChecked(), '  and sync is on');

  // 2. A fresh install signs in with the code, however it's typed; a typo is caught first.
  ({ page } = await launch(true));
  const before = seen.length;
  await signInTyping(page, words.slice(0, 8) + (words[8] === 'K' ? 'M' : 'K') + words.slice(9));
  await page.locator('.login .err').waitFor({ timeout: 10000 });
  const err = await page.locator('.login .err').textContent();
  const typoSent = seen.slice(before).some((r) => r.path === '/login');
  // A changed character passes the typo check 1 time in 16: then the server says no instead.
  ok(err.includes('isn\'t a valid account code') ? !typoSent : err.length > 0, 'a mistyped character doesn\'t sign in: ' + err.trim());
  // A real code that signs in to nothing: refused, and never sent (not even as a fallback).
  const nobody = newCode();
  const at = seen.length;
  await signInTyping(page, nobody);
  await page.locator('.login .err', { hasText: 'didn\'t sign in' }).waitFor({ timeout: 60000 });
  await sleep(1000);
  ok(seen.slice(at).filter((x) => x.path === '/login').length === 1 && !leaked(nobody), 'a code that signs in to nothing is refused, and never sent: ' + (await page.locator('.login .err').textContent()).trim());
  // Lower case, spaces instead of dashes, O for 0, I for 1.
  const messy = words.toLowerCase().replace(/-/g, ' ').replace(/0/g, 'o').replace(/1/g, 'I');
  await signInTyping(page, messy);
  await page.waitForSelector('#composer', { timeout: 60000 });
  ok(true, 'signing in with the code (lower case, spaces, O for 0, I for 1) works: ' + messy);
  ok(!leaked(words), '  without sending them');

  // 3. An account with a 16-digit number (made the old way, through the API with a login key).
  const digits = Array.from({ length: 16 }, () => randomInt(10)).join('');
  const r = await fetch(WEB + '/create_account', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ accept_tos: true, account_key: loginKey(digits) }) });
  ok(r.ok, '(setup: an account with a 16-digit number)');
  ({ page } = await launch(true));
  await signInTyping(page, digits.replace(/(\d{4})(?=\d)/g, '$1 '));
  await page.waitForSelector('#composer', { timeout: 60000 });
  ok(true, 'a 16-digit number still signs in');
  // It asks once whether to upgrade (encrypted sync needs a code).
  await page.locator('#code-upgrade').waitFor({ timeout: 20000 });
  const offer = await page.locator('#code-upgrade').textContent();
  ok(offer.includes('Enable encrypted sync?') && offer.includes('Upgrading to account code enables encrypted sync.') && offer.includes('How this works'), '  and asks "Enable encrypted sync?"');
  shot('code-offer');
  await page.locator('#code-upgrade-go').click();
  await page.locator('#code-upgrade .number').waitFor({ timeout: 20000 });
  ok(CODE.test(await wordsOn(page, '#code-upgrade .number')) && await page.locator('#composer').count() > 0, '  "Generate New Account Code" shows a new code to save in the same sheet, over the chat');
  ok(await page.locator('#code-upgrade-convert').isDisabled(), '  Convert waits until it\'s written down');
  await page.locator('#code-upgrade-cancel').click();
  ok(!(await page.locator('#code-upgrade').count()) && !seen.some((x) => x.path === '/account_number/rotate'), '  Cancel closes it and changes nothing');
  await toAccount(page);
  await page.locator('#sync-needs-words').waitFor({ timeout: 10000 });
  ok(await page.locator('#sync-needs-words').isVisible(), '  Account says sync needs an account code');
  ok((await page.locator('#rotate-words').textContent()).includes('Generate My Account Code'), '  and offers Generate My Account Code');
  shot('code-number-account');

  // Converting: the code comes first, Convert waits until it's saved, Cancel changes nothing.
  await page.locator('#rotate-words').click();
  await page.locator('#new-words .number').waitFor();
  ok(await page.locator('#switch-words').isDisabled(), 'Generate My Account Code shows the code, and Convert waits until it\'s saved');
  await page.locator('#new-words button.bare', { hasText: 'Cancel' }).click();
  ok(!seen.some((x) => x.path === '/account_number/rotate'), '  Cancel changes nothing');
  await page.locator('#rotate-words').click();
  const fresh = await wordsOn(page, '#new-words .number');
  await page.locator('#words-saved input').check();
  shot('code-convert');
  await page.locator('#switch-words').click();
  await page.locator('#new-words', { hasText: 'Done' }).waitFor({ timeout: 60000 });
  const rotate = seen.find((x) => x.path === '/account_number/rotate');
  ok(rotate && /^\{"account_key":"[0-9a-f]{64}"\}$/.test(rotate.body) && !leaked(fresh), 'Convert: only a key made from the new code was sent');
  await page.locator('#sync-on').waitFor({ timeout: 20000 });
  // (No chats here yet, so nothing to upload: it has started when it asks for the synced copy.)
  const rotatedAt = seen.indexOf(rotate);
  await until(async () => seen.slice(rotatedAt).some((x) => x.path.startsWith('/sync/v1/records')), 60000);
  ok(await page.locator('#sync-on').isChecked() && cat('account-kind.json').includes('code'), '  sync turned on, and the phone now has a code');
  shot('code-converted');
  // Sign-in is rate limited (429, retry after a few seconds): wait it out.
  const login = async (key) => {
    for (let i = 0; i < 12; i++) {
      const r = await fetch(WEB + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: key }) });
      if (r.status !== 429) return r.status;
      await sleep(1000 * (+(r.headers.get('retry-after') || 5) + 1));
    }
    return 429;
  };
  const oldStatus = await login(loginKey(digits));
  const newStatus = await login(loginKey(fresh));
  ok(oldStatus === 401 && newStatus === 200, `  the old number no longer signs in; the new code does (${oldStatus}, ${newStatus})`);

  console.log('\nall account code checks passed');
} finally {
  recorder.close();
}
process.exit(0);

async function until(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(1500); }
  throw new Error('FAILED: timed out');
}
