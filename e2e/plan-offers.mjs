// End-to-end test of the Account screen's plan list: it shows the plans and prices the server offers
// this account (asked with the account's key), and buying one charges exactly the price shown. Needs
// a dev server whose database is reachable through the dev Postgres container (to fund the account):
//   OBSCURIFY_API=http://127.0.0.1:8099 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8099 tcp:8099 && DB=<dev database> [CATALOG=<plan list>] node e2e/plan-offers.mjs
// On a plan list whose plans have weekly limits it also checks the usage bars in the menu, sync (off until
// a plan that has it), and that a chat past the limit is refused with which limit it is, never sent tied to
// the account. CATALOG puts the new account on another of the server's plan lists first.
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
  sql(`INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 400, 'deposit')`);
  const synced = sql(`SELECT p.chat_sync FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = '${id}'`) === 't';
  const [code, name, fee, weekly] = sql(`SELECT p.code, p.name, p.monthly_fee, COALESCE(p.weekly_credit::text, '') FROM catalog_plans c JOIN plans p ON p.id = c.plan_id WHERE c.catalog_id = '${catalog}' AND c.level = 'casual'`).split('|');
  const price = '$' + Number(fee).toFixed(2);

  await page.locator('.bar .icon').first().click();
  await page.locator('.foot button', { hasText: 'Account' }).click();
  await page.waitForSelector('.big');
  const row = page.locator('.section .row', { hasText: `${name} · ${price}/mo` });
  await row.waitFor({ timeout: 20000 });
  ok(true, `the plan list shows this account's ${name} at ${price}/mo`);
  ok(synced ? await page.locator('#sync-switch').count() === 1 : /comes with a plan/.test(await page.locator('#sync-plan-off').innerText()), synced ? 'sync is there' : 'sync is off until a plan that has it');
  // The server words each plan from its terms; a weekly limit is part of what it says.
  if (weekly) ok(new RegExp(`\\$${Number(weekly)}( of it)? a week`).test(await row.innerText()), `it says ${name}'s weekly limit: ${(await row.innerText()).replace(/\s+/g, ' ')}`);
  // A plan worded in parts (points, terms): each point on a line of its own, the terms under them, as on the web.
  {
    const lines = await row.locator('.point').allInnerTexts();
    const terms = await row.locator('.terms').allInnerTexts();
    if (weekly) ok(lines.length >= 2 && terms.length === 1 && /a week/.test(terms[0]) && lines.every((l) => !/a week/.test(l)),
      `${name}: ${lines.length} points on lines of their own (${lines.join(' / ')}), terms under them (${terms[0]})`);
  }
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
  if (weekly) {
    await page.waitForSelector('#sync-switch', { timeout: 20000 });
    ok(true, `${name} has sync`);
    // One chat of included usage: the menu's bar shows its share of the week, no dollars.
    const back = async () => { await page.locator('.bar .icon').first().click(); await page.waitForSelector('#composer'); };
    const send = async (text) => { await page.locator('#composer').fill(text); await page.locator('button.send').click(); };
    await back();
    await send('Say hi in two words.');
    for (let i = 0; i < 60 && Number(sql(`SELECT COALESCE(SUM(credit_used), 0) FROM monthly_usage WHERE user_id = '${id}'`)) === 0; i++) await sleep(500);
    const used = Number(sql(`SELECT week_used FROM usage_windows WHERE user_id = '${id}'`));
    ok(used >= 0.1 && Number(sql(`SELECT count(*) FROM invoices WHERE user_id = '${id}'`)) === 0, `a private chat took $${used} of included usage, and nothing was billed to the account by name`);
    await page.locator('.bar .icon').first().click();
    await page.waitForFunction((t) => [...document.querySelectorAll('.drawer .usage')].some((u) => u.innerText.trim().startsWith(t)), `This week: ${Math.round(100 * used / Number(weekly))}%`, { timeout: 30000 });
    ok(await page.locator('.drawer .bal').count() === 0, `the menu says ${(await page.locator('.drawer .usage').first().innerText()).trim()}, no dollars`);
    shot(`plan-offers-${catalog}-menu`);
    await page.locator('.veil').first().click({ position: { x: 380, y: 400 } });

    // A paid model (a free one answers whatever the limits say).
    await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 120000 });
    await page.locator('.title.model').click();
    await page.locator('input.search').fill(process.env.PAID_MODEL || 'Claude Sonnet');
    await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
    const ask = async (text) => {
      const n = await page.locator('.msg.assistant, .msg.err').count();
      await page.locator('#composer').fill(text);
      await sleep(300);
      await page.locator('.send').click();
      await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), n, { timeout: 90000 });
      return (await page.locator('.msg.assistant, .msg.err').last().innerText()).trim();
    };
    // Whatever goes into a session is taken away, so the phone has to fill its wallet again.
    const drain = () => sql(`UPDATE users SET balance = 0 WHERE is_session`);
    const fromBalance = () => Number(sql(`SELECT COALESCE(SUM(amount), 0) FROM transactions WHERE user_id = '${id}' AND transaction_type = 'voucher_withdrawal'`));

    // The 5 hours used up, money on the balance: the balance pays and the chat goes on.
    sql(`UPDATE usage_windows SET burst_used = 1000000 WHERE user_id = '${id}'`);
    let said = '';
    for (let i = 0; i < 5 && fromBalance() === 0; i++) { drain(); said = await ask(`Say hello (${i}).`); }
    ok(fromBalance() > 0 && said && !/used up|^Error/.test(said), `past the limit the balance pays ($${fromBalance().toFixed(2)}) and the chat answers: ${said.split('\n')[0].slice(0, 40)}`);

    // Nothing left: the chat says which limit, and the message is not sent tied to the account.
    sql(`UPDATE users SET balance = 0 WHERE id = '${id}'`);
    for (let i = 0; i < 12 && !/used up|you have left/.test(said); i++) { drain(); said = await ask(`Say hi again (${i}).`); }
    // The phone still counts what the drained sessions held (taking a session's money away on the
    // server is nothing it could know) until it closes them for their change, once they have been
    // idle for 5 minutes. New chats meanwhile don't keep them open.
    const emptied = Date.now();
    for (let i = 0; i < 16 && !/usage is used up/.test(said); i++) {
      await sleep(30000);
      await page.locator('.bar .icon', { hasText: '＋' }).click().catch(() => {});
      await sleep(700);
      said = await ask(`Say hi once more (${i}).`);
    }
    console.log(`  the phone knew it held nothing after ${Math.round((Date.now() - emptied) / 1000)} s`);
    ok(/5-hour usage is used up\. It starts over/.test(said), `with no balance the chat says which limit and when it starts over: ${said.replace(/\s+/g, ' ').slice(0, 160)}`);
    ok(Number(sql(`SELECT count(*) FROM invoices WHERE user_id = '${id}'`)) === 0, 'and nothing was sent tied to the account');
    shot(`plan-offers-${catalog}-limit`);
  }
  console.log('all ok');
}
process.exit(0);
