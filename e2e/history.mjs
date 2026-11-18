// Transaction history on the Account screen, like the web's: spending per day, week and month,
// tap a period for its charges, every charge under All, all paged on the server; money moved into
// the private wallet isn't listed (it's still the customer's money).
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
await page.getByRole('button', { name: 'Create Account' }).click();
await page.locator('.words').waitFor({ timeout: 30000 });
const userId = sql(`SELECT id FROM users ORDER BY created_at DESC LIMIT 1`);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
sql(`INSERT INTO transactions (user_id, amount, transaction_type, created_at) VALUES
  ('${userId}', 10, 'deposit', NOW() - INTERVAL '2 days'),
  ('${userId}', 1, 'voucher_withdrawal', NOW()),
  ('${userId}', 5, 'subscription', NOW());
  INSERT INTO transactions (user_id, amount, transaction_type, created_at)
  SELECT '${userId}', 0.0015, 'withdrawal', NOW() - INTERVAL '2 days' + (g || ' seconds')::interval FROM generate_series(1, 60) g;
  INSERT INTO transaction_logs (transaction_id, previous_balance, new_balance, notes)
  SELECT id, 0, 10, 'Deposit: +10.00 (card top-up)' FROM transactions WHERE user_id = '${userId}' AND transaction_type = 'deposit';`);

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
ok(!/prepaid|wallet/i.test(today) && /plan \$5\.00/.test(today) && /\$5\.00/.test(today), 'today: the plan fee spent, the move to the wallet not shown: ' + today);
const older = await days.nth(1).textContent();
ok(/60 requests/.test(older) && /\$0\.09/.test(older) && /\+\$10\.00/.test(older), 'two days ago: 60 requests, $0.09 spent, +$10 added: ' + older);
ok(await hist.locator('.meter div').count() === 2, 'spend bars drawn');
shot('history-daily');

// 3. Today's charges: labelled like the web, signed, coloured.
await days.first().click();
await hist.locator('.history-head').waitFor();
const charges = await hist.textContent();
ok(!/prepaid|wallet/i.test(charges), 'no move to a wallet among the charges');
ok(/Plan fee.*−\$5\.00/.test(charges), 'Plan fee −$5.00');
ok(await hist.locator('.debit').count() === 1, 'debits in red');
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
const dep = hist.locator('.row', { hasText: 'Deposit' });
ok(await dep.locator('.detail').count() === 0, 'its payment details stay folded');
await dep.click();
ok(/Paid with card/.test(await dep.locator('.detail').textContent()), 'tapped: Paid with card');
shot('history-page2');

// 5. Weekly and Monthly group them; All lists every charge.
await hist.getByRole('button', { name: 'Monthly' }).click();
await page.waitForFunction(() => document.querySelectorAll('.history .row.tap').length >= 1 && /\d{4}/.test(document.querySelector('.history .row.tap').textContent));
ok(!/prepaid|wallet/i.test(await days.first().textContent()), 'Monthly says nothing about wallets');
await hist.getByRole('button', { name: 'All' }).click();
const total = sql(`SELECT COUNT(*) FROM transactions WHERE user_id = '${userId}' AND transaction_type <> 'voucher_withdrawal'`);
await page.waitForFunction((t) => new RegExp('1–50 of ' + t).test(document.querySelector('.history .pager')?.textContent || ''), total);
ok(true, `All: every charge, paged (${total})`);
shot('history-all');
console.log('all history checks passed');
process.exit(0);
