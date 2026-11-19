// Prod: every route on the phone from an old 16-digit number to an account code, against
// obscurify.ai. Build first with the default (prod) API: DEVTOOLS=1 ./build.sh x86_64.
// Accounts are made over Tor (prod's sign-up limit); old/new credentials are checked over Tor too.
import { execFileSync } from 'node:child_process';
import { randomInt } from 'node:crypto';
import { launch, adb, ok, shot, PKG } from './lib.mjs';
import { accountKey } from '../../obscurify-cli/lib/account.mjs';

const BASE = 'https://obscurify.ai';
let n = 0;
const tor = (path, body) => { n++; return execFileSync('curl', ['-s', '-m', '90', '--socks5-hostname', '127.0.0.1:9050', '-U', `phone${n}${Date.now()}:x`, '-o', '/dev/null', '-w', '%{http_code}', '-H', 'Content-Type: application/json', '-d', JSON.stringify(body), BASE + path]).toString(); };
const files = `/data/user/0/${PKG}/files`;
function numberAccount() {
  const num = Array.from({ length: 16 }, () => randomInt(10)).join('');
  const code = tor('/create_account', { accept_tos: true, account_key: accountKey(num) });
  if (code !== '201') throw new Error('create_account over Tor: ' + code);
  return num;
}
const signIn = async (page, text) => {
  await page.locator('.login input').fill(text);
  await page.locator('.login button.go').click();
  await page.waitForSelector('#composer', { timeout: 90000 });
};
const offered = (page) => page.locator('#code-upgrade').waitFor({ timeout: 20000 }).then(() => true, () => false);
async function convertInSheet(page, route, num) {
  await page.locator('#code-upgrade-go').click();
  await page.locator('#code-upgrade .number').waitFor({ timeout: 30000 });
  const made = (await page.locator('#code-upgrade .number').textContent()).trim();
  ok(/^[0-9A-Z]{5}(-[0-9A-Z]{5}){3}$/.test(made), `${route}: the sheet shows a new code (${made})`);
  ok(await page.locator('#code-upgrade-convert').isDisabled(), `${route}:   Convert waits until it's written down`);
  await page.locator('#code-upgrade-saved input').check();
  await page.locator('#code-upgrade-convert').click();
  await page.locator('#code-upgrade-done').waitFor({ timeout: 90000 });
  ok((await page.locator('#code-upgrade h2').textContent()).includes('Encrypted sync is on'), `${route}:   Convert finishes in the sheet`);
  await page.locator('#code-upgrade-done').click();
  ok(await page.locator('#composer').count() > 0, `${route}:   still on the chat`);
  return made;
}
function credentialsMoved(route, num, made) {
  ok(tor('/generate_token', { account_key: accountKey(made) }) === '200', `${route}:   the new code signs in`);
  ok(tor('/generate_token', { account_key: accountKey(num) }) !== '200', `${route}:   the old number no longer does`);
}

try {
  // 1. Signing in with the number: the sheet.
  let num = numberAccount();
  let { page } = await launch(true);
  await signIn(page, num.replace(/(\d{4})(?=\d)/g, '$1 '));
  ok(await offered(page), 'Route 1 (sign in with the number): asked');
  shot('prod-route1');
  credentialsMoved('Route 1', num, await convertInSheet(page, 'Route 1', num));

  // 2. A phone already signed in that never saw the offer: asked on launch.
  num = numberAccount();
  ({ page } = await launch(true));
  await signIn(page, num);
  await offered(page);
  await page.locator('#code-upgrade-later').click();
  adb('shell', 'rm', '-f', `${files}/code-offer-seen.json`);
  ({ page } = await launch(false));
  ok(await offered(page), 'Route 2 (already signed in, on launch): asked');
  credentialsMoved('Route 2', num, await convertInSheet(page, 'Route 2', num));

  // 3. The Account screen's "Generate My Account Code".
  num = numberAccount();
  ({ page } = await launch(true));
  await signIn(page, num);
  await offered(page);
  await page.locator('#code-upgrade-later').click();
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#rotate-words').waitFor({ timeout: 20000 });
  await page.locator('#rotate-words').click();
  await page.locator('#new-words .number').waitFor({ timeout: 30000 });
  const made = (await page.locator('#new-words .number').textContent()).trim();
  ok(await page.locator('#switch-words').isDisabled(), `Route 3 (Account screen): a new code (${made}), Convert waits`);
  await page.locator('#words-saved input').check();
  await page.locator('#switch-words').click();
  await page.locator('#new-words', { hasText: 'Done' }).waitFor({ timeout: 90000 });
  ok(true, 'Route 3:   converted on the Account screen');
  shot('prod-route3');
  credentialsMoved('Route 3', num, made);
  // 4. Another device converts the account: this phone stays signed in and asks for the new code.
  num = numberAccount();
  ({ page } = await launch(true));
  await signIn(page, num);
  await offered(page);
  await page.locator('#code-upgrade-later').click();
  const jar = `/tmp/prod-route4-${Date.now()}.jar`;
  const viaTor = (args) => execFileSync('curl', ['-s', '-m', '90', '--socks5-hostname', '127.0.0.1:9050', '-U', `phone4${Date.now()}:x`, ...args]).toString();
  viaTor(['-c', jar, '-o', '/dev/null', '-H', 'Content-Type: application/json', '-d', JSON.stringify({ account_key: accountKey(num) }), BASE + '/login']);
  const { newAccountCode } = await import('../../obscurify-cli/lib/account.mjs');
  const elsewhere = newAccountCode();
  const rotated = viaTor(['-b', jar, '-o', '/dev/null', '-w', '%{http_code}', '-H', 'Content-Type: application/json', '-d', JSON.stringify({ account_key: accountKey(elsewhere) }), BASE + '/account_number/rotate']);
  ok(rotated === '200', `Route 4 (another device converted the account): ${elsewhere}`);
  ({ page } = await launch(false));
  await page.locator('#enter-code').waitFor({ timeout: 30000 });
  ok(await page.locator('#composer').count() > 0 && !(await page.locator('#code-upgrade').count()), 'Route 4:   this phone stays signed in and asks for the code, not to make another');
  shot('prod-route4');
  await page.locator('#enter-code-input').fill(elsewhere);
  await page.locator('#enter-code-go').click();
  await page.locator('#enter-code').waitFor({ state: 'detached', timeout: 90000 });
  ok(true, 'Route 4:   entering it signs in with it');

  console.log('\nall prod phone routes passed');
} catch (e) {
  console.error('FAILED: ' + e.message);
  process.exitCode = 1;
} finally {
  process.exit(process.exitCode || 0);
}
