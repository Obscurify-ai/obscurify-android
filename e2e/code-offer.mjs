// The account code upgrade (main.rs CodeUpgrade) is offered once per phone to anyone signed in
// without a code: right after signing in with a number, and on launch for a phone that was already
// signed in (an update from before the offer); then only the Account screen offers it.
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8232 tcp:8232
//   UPSTREAM_PORT=8098 node code-offer.mjs
import http from 'node:http';
import { randomInt } from 'node:crypto';
import { launch, adb, ok, shot, PKG } from './lib.mjs';
import { accountKey, newAccountCode } from '../../obscurify-cli/lib/account.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8098) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const proxy = http.createServer((req, res) => {
  const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on('error', () => res.destroy());
  req.pipe(up);
});
await new Promise((r) => proxy.listen(8232, '127.0.0.1', r));
adb('reverse', 'tcp:8232', 'tcp:8232');
const files = `/data/user/0/${PKG}/files`;
const signIn = async (page, text) => {
  await page.locator('.login input').fill(text);
  await page.locator('.login button.go').click();
  await page.waitForSelector('#composer', { timeout: 60000 });
};
const offered = (page) => page.locator('#code-upgrade').waitFor({ timeout: 10000 }).then(() => true, () => false);
const create = (key) => fetch(WEB + '/create_account', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ accept_tos: true, account_key: key }) });

try {
  // 1. Signing in with a number: asked.
  const digits = Array.from({ length: 16 }, () => randomInt(10)).join('');
  ok((await create(accountKey(digits))).ok, '(setup: an account with a 16-digit number)');
  let { page } = await launch(true);
  await signIn(page, digits);
  ok(await offered(page), 'signing in with a number asks once');
  shot('code-offer');
  await page.locator('#code-upgrade-later').click();
  ok(!(await page.locator('#code-upgrade').count()), '  Not now closes it');

  // 2. A phone already signed in that never saw the offer (updated from before it): asked on launch.
  adb('shell', 'rm', '-f', `${files}/code-offer-seen.json`);
  ({ page } = await launch(false));
  ok(await offered(page), 'a phone already signed in with a number is asked on launch');
  await page.locator('#code-upgrade-later').click();
  ({ page } = await launch(false));
  ok(!(await offered(page)), '  and only once');
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  ok(await page.locator('#sync-upgrade').isVisible().catch(() => false) || await page.locator('#rotate-words').isVisible(), '  Account still offers the upgrade');

  // 3. Converting right in the sheet: Generate, write it down, Convert, Done. No other screen.
  const conv = Array.from({ length: 16 }, () => randomInt(10)).join('');
  await create(accountKey(conv));
  ({ page } = await launch(true));
  await signIn(page, conv);
  ok(await offered(page), '(another number account, asked)');
  await page.locator('#code-upgrade-go').click();
  await page.locator('#code-upgrade .number').waitFor({ timeout: 20000 });
  const made = (await page.locator('#code-upgrade .number').textContent()).trim();
  ok(/^[0-9A-Z]{5}(-[0-9A-Z]{5}){3}$/.test(made), 'Generate New Account Code shows the code in the sheet: ' + made);
  ok(await page.locator('#code-upgrade-convert').isDisabled(), '  Convert waits until it is written down');
  await page.locator('#code-upgrade-saved input').check();
  shot('code-offer-generate');
  await page.locator('#code-upgrade-convert').click();
  await page.locator('#code-upgrade-done').waitFor({ timeout: 60000 });
  ok((await page.locator('#code-upgrade h2').textContent()).includes('Encrypted sync is on'), 'Convert switches the account in the sheet');
  ok(/stay signed in/.test(await page.locator('#code-upgrade p').first().textContent()) && /API keys keep working/.test(await page.locator('#code-upgrade p').first().textContent()), '  it says other devices stay signed in and API keys keep working');
  ok(!(await page.locator('#code-upgrade button', { hasText: /^Copy$/ }).count()), '  no separate Copy button: tapping the code copies it');
  await page.locator('#code-upgrade-done').click();
  ok(!(await page.locator('#code-upgrade').count()) && await page.locator('#composer').count() > 0, '  Done closes it, still on the chat');
  const login = (key) => fetch(WEB + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: key }) }).then((r) => r.ok);
  ok(await login(accountKey(made)) && !(await login(accountKey(conv))), '  the new code signs in, the old number no longer does');

  // 4. The account gets its code on another device: this phone stays signed in, asks for the code
  //    (never offers to make another), and syncs once it's entered. Tapping a code copies it.
  const other = Array.from({ length: 16 }, () => randomInt(10)).join('');
  await create(accountKey(other));
  ({ page } = await launch(true));
  await signIn(page, other);
  await offered(page);
  await page.locator('#code-upgrade-later').click();
  // "Another device": signs in with the number and converts it.
  const signInWeb = await fetch(WEB + '/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: accountKey(other) }) });
  const cookie = (signInWeb.headers.getSetCookie() || []).map((c) => c.split(';')[0]).find((c) => c.startsWith('session='));
  const elsewhere = newAccountCode();
  const rotated = await fetch(WEB + '/account_number/rotate', { method: 'POST', headers: { 'Content-Type': 'application/json', Cookie: cookie }, body: JSON.stringify({ account_key: accountKey(elsewhere) }) });
  ok(rotated.ok, '(another device converted the account: ' + elsewhere + ')');
  ({ page } = await launch(false));
  await page.locator('#enter-code').waitFor({ timeout: 20000 });
  ok(await page.locator('#composer').count() > 0 && !(await page.locator('#code-upgrade').count()), 'this phone stays signed in and asks for the code, without offering to make another');
  shot('enter-code');
  await page.locator('#enter-code-input').fill(elsewhere.toLowerCase());
  await page.locator('#enter-code-go').click();
  await page.locator('#enter-code').waitFor({ state: 'detached', timeout: 60000 });
  ok(true, '  entering it signs in with it here');
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#sync-on').waitFor({ timeout: 20000 });
  ok(await page.locator('#sync-on').isChecked(), '  and this phone syncs');

  // 5. A code account is never asked.
  const code = newAccountCode();
  await create(accountKey(code));
  ({ page } = await launch(true));
  await signIn(page, code);
  ok(!(await offered(page)), 'a code account is never asked');
  console.log('\nall code offer checks passed');
} catch (e) {
  console.error(e.message);
  process.exitCode = 1;
} finally {
  proxy.close();
  adb('reverse', '--remove', 'tcp:8232');
  process.exit(process.exitCode || 0);
}
