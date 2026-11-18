// Account words on the phone (private.rs, api.rs, main.rs): sign-up makes 12 words on the phone and
// sends only a key made from them; they sign back in however they're typed; a typo is caught on the
// phone; an account with a 16-digit number signs in, can't sync, and switches to words (saved first),
// which turns sync on; the old number stops working.
//
// The app talks to a recorder on :8232 that forwards to the website's dev server, so the test sees
// what left the phone:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8232 tcp:8232
//   UPSTREAM_PORT=8097 node words.mjs
import http from 'node:http';
import { randomInt } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, PKG } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const CLI = process.env.SESSIONS_BIN || `${process.env.HOME}/Work/unlinkable-inference-words/target/release/unlinkable-inference`;
const loginKey = (input) => execFileSync(CLI, ['login-key'], { input }).toString().trim();
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
const wordsOn = async (page, sel) => (await page.locator(`${sel} span`).allTextContents()).map((t) => t.replace(/^\d+\.\s*/, '').trim()).join(' ');
const leaked = (words) => seen.some((r) => r.body.includes(words) || words.split(' ').some((w, i, a) => i < 10 && r.body.includes(`${w} ${a[i + 1]} ${a[i + 2]}`)));
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
  // 1. Sign up: 12 words, made on the phone.
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Make My Account Words' }).click();
  await page.locator('.words').waitFor({ timeout: 30000 });
  const words = await wordsOn(page, '.words');
  ok(/^([a-z]+ ){11}[a-z]+$/.test(words), 'sign-up shows 12 account words: ' + words);
  shot('words-signup');
  const created = seen.find((r) => r.path === '/create_account');
  ok(created && /"account_key":"[0-9a-f]{64}"/.test(created.body) && !leaked(words), '  only a key made from them left the phone');
  ok(cat('account-kind.json').includes('words'), '  the phone knows it signed in with words');
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  await toAccount(page);
  ok(await page.locator('#sync-on').isChecked(), '  and sync is on');

  // 2. A fresh install signs in with the words, however they're typed; a typo is caught first.
  ({ page } = await launch(true));
  const list = words.split(' ');
  const before = seen.length;
  await signInTyping(page, list.slice(0, 11).concat([list[11] === 'zoo' ? 'abandon' : 'zoo']).join(' '));
  await page.locator('.login .err').waitFor({ timeout: 10000 });
  const err = await page.locator('.login .err').textContent();
  const typoSent = seen.slice(before).some((r) => r.path === '/login');
  // A different last word passes the checksum 1 time in 16: then the server says no instead.
  ok(err.includes('aren\'t 12 account words') ? !typoSent : err.length > 0, 'a wrong word doesn\'t sign in: ' + err.trim());
  // Real words that sign in to nothing: refused, and never sent (not even as a fallback).
  const nobody = 'legal winner thank year wave sausage worth useful legal winner thank yellow';
  const at = seen.length;
  await signInTyping(page, nobody);
  await page.locator('.login .err', { hasText: 'didn\'t sign in' }).waitFor({ timeout: 60000 });
  await sleep(1000);
  ok(seen.slice(at).filter((x) => x.path === '/login').length === 1 && !leaked(nobody), 'words that sign in to nothing are refused, and never sent: ' + (await page.locator('.login .err').textContent()).trim());
  const messy = list.map((w, i) => `${i + 1}. ${i % 3 === 0 ? w.toUpperCase() : i % 3 === 1 && w.length > 4 ? w.slice(0, 4) : w}`).join(', ');
  await signInTyping(page, messy);
  await page.waitForSelector('#composer', { timeout: 60000 });
  ok(true, 'signing in with the words (numbered, capitals, short forms) works');
  ok(!leaked(words), '  without sending them');

  // 3. An account with a 16-digit number (made the old way, through the API with a login key).
  const digits = Array.from({ length: 16 }, () => randomInt(10)).join('');
  const r = await fetch(WEB + '/create_account', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ accept_tos: true, account_key: loginKey(digits) }) });
  ok(r.ok, '(setup: an account with a 16-digit number)');
  ({ page } = await launch(true));
  await signInTyping(page, digits.replace(/(\d{4})(?=\d)/g, '$1 '));
  await page.waitForSelector('#composer', { timeout: 60000 });
  ok(true, 'a 16-digit number still signs in');
  await toAccount(page);
  ok(await page.locator('#sync-needs-words').isVisible(), '  Account says sync needs account words');
  ok((await page.locator('#rotate-words').textContent()).includes('Switch to Words'), '  and offers Switch to Words');
  shot('words-number-account');

  // Switching: the words come first, Switch waits until they're saved, Cancel changes nothing.
  await page.locator('#rotate-words').click();
  await page.locator('#new-words .words').waitFor();
  ok(await page.locator('#switch-words').isDisabled(), 'Switch to Words shows 12 words, and Switch waits until they\'re saved');
  await page.locator('#new-words button.bare', { hasText: 'Cancel' }).click();
  ok(!seen.some((x) => x.path === '/account_number/rotate'), '  Cancel changes nothing');
  await page.locator('#rotate-words').click();
  await page.locator('#new-words .words').waitFor();
  const fresh = await wordsOn(page, '#new-words .words');
  await page.locator('#words-saved input').check();
  shot('words-switch');
  await page.locator('#switch-words').click();
  await page.locator('#new-words', { hasText: 'Done' }).waitFor({ timeout: 60000 });
  const rotate = seen.find((x) => x.path === '/account_number/rotate');
  ok(rotate && /^\{"account_key":"[0-9a-f]{64}"\}$/.test(rotate.body) && !leaked(fresh), 'Switch: only a key made from the new words was sent');
  await page.locator('#sync-on').waitFor({ timeout: 20000 });
  // (No chats here yet, so nothing to upload: it has started when it asks for the synced copy.)
  const rotatedAt = seen.indexOf(rotate);
  await until(async () => seen.slice(rotatedAt).some((x) => x.path.startsWith('/sync/v1/records')), 60000);
  ok(await page.locator('#sync-on').isChecked() && cat('account-kind.json').includes('words'), '  sync turned on, and the phone now has words');
  shot('words-switched');
  const oldOk = (await fetch(WEB + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: loginKey(digits) }) })).ok;
  const newOk = (await fetch(WEB + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: loginKey(fresh) }) })).ok;
  ok(!oldOk && newOk, '  the old number no longer signs in; the new words do');

  console.log('\nall words checks passed');
} finally {
  recorder.close();
}
process.exit(0);

async function until(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(1500); }
  throw new Error('FAILED: timed out');
}
