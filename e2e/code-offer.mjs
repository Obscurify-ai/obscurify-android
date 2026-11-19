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

  // 3. A code account is never asked.
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
