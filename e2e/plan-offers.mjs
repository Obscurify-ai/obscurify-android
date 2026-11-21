// End-to-end test of the Account screen's plan list: it shows the plans and prices the server offers
// this account (asked with the account's key), and buying one charges exactly the price shown. Needs
// a dev server whose database is reachable through the dev Postgres container (to fund the account):
//   OBSCURIFY_API=http://127.0.0.1:8099 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8099 tcp:8099 && DB=<dev database> [CATALOG=<plan list>] node e2e/plan-offers.mjs
// CATALOG puts the new account on another of the server's plan lists first.
import { execFileSync } from 'node:child_process';
import { launch, ok, shot, sleep, wordsOn } from './lib.mjs';

const DB = process.env.DB || 'obscurify';
const CATALOG = (process.env.CATALOG || '').replace(/[^a-z0-9-]/g, '');
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-F', '|', '-c', q], { encoding: 'utf8' }).trim();

{
  const { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Create Account' }).click();
  await wordsOn(page, '.number', 20000);
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');

  const id = sql("SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1");
  if (CATALOG) sql(`UPDATE users SET catalog_id = '${CATALOG}', plan_id = (SELECT plan_id FROM catalog_plans WHERE catalog_id = '${CATALOG}' AND level = 'pay_as_you_go') WHERE id = '${id}'`);
  const catalog = sql(`SELECT catalog_id FROM users WHERE id = '${id}'`);
  ok(!CATALOG || catalog === CATALOG, `the new account buys from ${catalog}`);
  sql(`UPDATE users SET balance = balance + 400, pro_access_until = NULL WHERE id = '${id}'`);
  const [code, name, fee] = sql(`SELECT p.code, p.name, p.monthly_fee FROM catalog_plans c JOIN plans p ON p.id = c.plan_id WHERE c.catalog_id = '${catalog}' AND c.level = 'casual'`).split('|');
  const price = '$' + Number(fee).toFixed(2);

  await page.locator('.bar .icon').first().click();
  await page.locator('.foot button', { hasText: 'Account' }).click();
  await page.waitForSelector('.big');
  const row = page.locator('.section .row', { hasText: `${name} · ${price}/mo` });
  await row.waitFor({ timeout: 20000 });
  ok(true, `the plan list shows this account's ${name} at ${price}/mo`);
  shot(`plan-offers-${catalog}`);

  const before = Number(sql(`SELECT balance FROM users WHERE id = '${id}'`));
  await row.getByRole('button', { name: /Upgrade|Switch/ }).click();
  const pay = page.getByRole('button', { name: `Pay ${price}` });
  await pay.waitFor({ timeout: 10000 });
  ok(true, `it asks to pay ${price}`);
  await pay.click();
  for (let i = 0; i < 40 && sql(`SELECT account_tier FROM users WHERE id = '${id}'`) !== 'casual'; i++) await sleep(500);
  const [tier, balance, held] = sql(`SELECT u.account_tier, u.balance, p.code FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = '${id}'`).split('|');
  ok(tier === 'casual' && held === code, `the account holds ${held}`);
  ok(Math.abs(before - Number(balance) - Number(fee)) < 0.000001, `charged exactly ${price} (${before} -> ${balance})`);
  await page.waitForSelector('.section .row .ok:has-text("Current")', { timeout: 20000 });
  shot(`plan-offers-${catalog}-bought`);
  console.log('all ok');
}
process.exit(0);
