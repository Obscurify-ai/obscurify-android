// Transaction history on the Account screen, like the web's: spending per day, week and month,
// tap a period for its charges, every charge under All, all paged on the server; money moved into
// the private wallet shows as leaving the balance, labelled, and never as spending.
//   OBSCURIFY_API=http://127.0.0.1:8230 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8230 tcp:8230; DB=obscurify_unlinkable node e2e/history.mjs
// Writes ledger rows straight into the dev database; spends nothing.
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot } from './lib.mjs';

const DB = process.env.DB || 'obscurify_unlinkable';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
adb('reverse', 'tcp:8230', 'tcp:8230');

// 1. A new account, then a ledger: two days ago a deposit and 60 usage charges (more than a page),
// today $1 moved into the private wallet and a plan fee.
let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Generate Account Number' }).click();
await page.locator('.number').waitFor({ timeout: 30000 });
const userId = sql(`SELECT id FROM users ORDER BY created_at DESC LIMIT 1`);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
sql(`INSERT INTO transactions (user_id, amount, transaction_type, created_at) VALUES
  ('${userId}', 10, 'deposit', NOW() - INTERVAL '2 days'),
  ('${userId}', 1, 'voucher_withdrawal', NOW()),
  ('${userId}', 5, 'subscription', NOW());
  INSERT INTO transactions (user_id, amount, transaction_type, created_at)
  SELECT '${userId}', 0.0015, 'withdrawal', NOW() - INTERVAL '2 days' + (g || ' seconds')::interval FROM generate_series(1, 60) g;`);

// 2. Account: Daily first, newest day on top.
await page.locator('.bar .icon').first().click();
await page.getByText('Account', { exact: false }).last().click();
const hist = page.locator('.history');
await hist.scrollIntoViewIfNeeded();
await hist.locator('.row.tap').first().waitFor({ timeout: 20000 });
ok(await hist.locator('.seg button.on').textContent() === 'Daily', 'opens on Daily');
const days = hist.locator('.row.tap');
ok(await days.count() === 2, 'one row per active day: ' + await days.count());
const today = await days.first().textContent();
ok(/\$1\.00 prepaid for private chats/.test(today) && /plan \$5\.00/.test(today) && /\$5\.00/.test(today), 'today: plan fee spent, $1 to the wallet (not spent): ' + today);
const older = await days.nth(1).textContent();
ok(/60 requests/.test(older) && /\$0\.09/.test(older) && /\+\$10\.00/.test(older), 'two days ago: 60 requests, $0.09 spent, +$10 added: ' + older);
ok(await hist.locator('.meter div').count() === 2, 'spend bars drawn');
shot('history-daily');

// 3. Today's charges: labelled like the web, signed, coloured.
await days.first().click();
await hist.locator('.history-head').waitFor();
const charges = await hist.textContent();
ok(/Prepaid for private chats.*−\$1\.00/.test(charges), "Prepaid for private chats −$1.00");
ok(/Plan fee.*−\$5\.00/.test(charges), 'Plan fee −$5.00');
ok(await hist.locator('.debit').count() === 2, 'debits in red');
shot('history-today');
await hist.getByRole('button', { name: 'Back' }).click();

// 4. The busy day pages: 50 charges, then the rest.
await days.nth(1).click();
await hist.locator('.history-head').waitFor();
await page.waitForFunction(() => /1–50 of 61/.test(document.querySelector('.history .pager').textContent));
ok(await hist.getByText('Usage', { exact: true }).count() === 50, 'first page: 50 usage charges');
await hist.getByRole('button', { name: 'Older' }).click();
await page.waitForFunction(() => /51–61 of 61/.test(document.querySelector('.history .pager').textContent));
ok(await hist.getByText('Deposit', { exact: true }).count() === 1, 'second page ends with the deposit');
shot('history-page2');

// 5. Weekly and Monthly group them; All lists every charge.
await hist.getByRole('button', { name: 'Monthly' }).click();
await page.waitForFunction(() => document.querySelectorAll('.history .row.tap').length >= 1 && /\d{4}/.test(document.querySelector('.history .row.tap').textContent));
ok(/prepaid for private chats/.test(await days.first().textContent()), "Monthly shows the prepaid money");
await hist.getByRole('button', { name: 'All' }).click();
const total = sql(`SELECT COUNT(*) FROM transactions WHERE user_id = '${userId}'`);
await page.waitForFunction((t) => new RegExp('1–50 of ' + t).test(document.querySelector('.history .pager')?.textContent || ''), total);
ok(true, `All: every charge, paged (${total})`);
shot('history-all');
console.log('all history checks passed');
process.exit(0);
