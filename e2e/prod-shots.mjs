// Screenshots of the phone switching a prod account from an old number to an account code.
import { execFileSync } from 'node:child_process';
import { randomInt } from 'node:crypto';
import { launch, sleep } from './lib.mjs';
import { accountKey } from '../../obscurify-cli/lib/account.mjs';
const OUT = '/home/tony/Work/obscurify/screenshots/prod-app-';
const shot = (name) => execFileSync('sh', ['-c', `adb exec-out screencap -p > ${OUT}${name}.png`]);
const num = Array.from({ length: 16 }, () => randomInt(10)).join('');
const made = execFileSync('curl', ['-s', '-m', '90', '--socks5-hostname', '127.0.0.1:9050', '-U', `shots${Date.now()}:x`, '-o', '/dev/null', '-w', '%{http_code}', '-H', 'Content-Type: application/json', '-d', JSON.stringify({ accept_tos: true, account_key: accountKey(num) }), 'https://obscurify.ai/create_account']).toString();
if (made !== '201') throw new Error('create_account ' + made);
try {
  const { page } = await launch(true);
  await page.locator('.login input').fill(num.replace(/(\d{4})(?=\d)/g, '$1 '));
  await sleep(500);
  shot('1-sign-in-with-old-number');
  await page.locator('.login button.go').click();
  await page.locator('#code-upgrade').waitFor({ timeout: 60000 });
  await sleep(800);
  shot('2-asked');
  await page.locator('#code-upgrade-go').click();
  await page.locator('#code-upgrade .number').waitFor({ timeout: 30000 });
  await sleep(800);
  shot('3-new-code');
  await page.locator('#code-upgrade-saved input').check();
  await sleep(500);
  shot('4-written-down');
  await page.locator('#code-upgrade-convert').click();
  await page.locator('#code-upgrade-done').waitFor({ timeout: 90000 });
  await sleep(800);
  shot('5-done');
  await page.locator('#code-upgrade-done').click();
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#sync-on').waitFor({ timeout: 30000 });
  await sleep(1500);
  shot('6-account-shows-code');
  console.log('sync on in Account:', await page.locator('#sync-on').isChecked());
} finally {
  process.exit(0);
}
