// An account that moves to Pro gets the plan's models in the app at once, on private sessions
// (contact form 2026-11-20: "with a PRO plan I can't use Claude Opus 5.5, it says I don't have
// PRO/Casual"). Before unlinkable-inference 0.1.6 the wallet (shared by every device with the
// account's code) kept the Pay As You Go passes it got before the upgrade, so new sessions opened
// on the old plan and frontier models were refused.
//
// - Pay As You Go: a chat works, and the wallet holds Pay As You Go passes.
// - Upgrade to Pro (in the DB, as billing does), restart the app: a frontier model answers.
// - Wipe the app and sign in again (the wallet comes back from the code, old passes included):
//   a frontier model answers on the first chat.
//
//   OBSCURIFY_API=http://127.0.0.1:8231 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8231 tcp:8230; DB=obscurify_upgrade node e2e/upgrade.mjs
// Needs the dev Postgres container and a dev server with private sessions on. Spends a few cents.
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG, wordsOn } from './lib.mjs';

const DB = process.env.DB || 'obscurify_upgrade';
const FRONTIER = process.env.FRONTIER || 'Claude Haiku 5.5';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const files = `/data/user/0/${PKG}/files`;
const passTiers = () => {
  let passes = [];
  try { passes = JSON.parse(adb('shell', 'cat', `${files}/unlinkable-state.json`)).wallet.passes || []; } catch {}
  const n = {};
  for (const p of passes) n[p.tier] = (n[p.tier] || 0) + 1;
  return n;
};

const pick = async (page, search, name) => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(search);
  await page.locator('.pick', { hasText: name }).filter({ hasNotText: 'Thinking' }).first().locator('.grow').click();
};
const ask = async (page, text, timeout = 240000) => {
  const before = await page.locator('.msg.assistant, .msg.err').count();
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), before, { timeout });
  return (await page.locator('.msg.assistant, .msg.err').last().textContent()).trim();
};
const newChat = async (page) => { await page.locator('.bar .icon', { hasText: '＋' }).click(); await sleep(500); };

// 1. Sign up; fund the account as plain Pay As You Go (no starter Pro trial).
let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
const code = await wordsOn(page, '.number', 30000);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
const userId = sql(`SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1`);
sql(`UPDATE users SET balance = balance + 5, pro_access_until = NULL WHERE id = '${userId}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${userId}', 5, 'deposit')`);
// Restart so the app (and its wallet) see the plan as it is now.
({ page } = await launch(false));
await agentSwitches(page, false);
await pick(page, 'DeepSeek V4.1 Flash', 'DeepSeek: DeepSeek V4.1 Flash');
const a1 = await ask(page, 'Reply with exactly the word: pineapple');
ok(/pineapple/i.test(a1), 'Pay As You Go chat answered: ' + a1.slice(0, 40));
let tiers = {};
for (let i = 0; i < 20 && !tiers.pay_as_you_go; i++) { await sleep(1000); tiers = passTiers(); }
ok((tiers.pay_as_you_go || 0) >= 10 && !tiers.pro, 'the wallet holds Pay As You Go passes: ' + JSON.stringify(tiers));
shot('upgrade-payg');

// 2. Upgrade to Pro, restart the app: the frontier model answers.
sql(`UPDATE users SET account_tier = 'pro', subscription_started_at = NOW(), last_subscription_charge = NOW() WHERE id = '${userId}'`);
({ page } = await launch(false));
await newChat(page);
await pick(page, FRONTIER, FRONTIER);
const a2 = await ask(page, 'Reply with exactly the word: mango');
ok(/mango/i.test(a2) && !/plan/i.test(a2), 'after the upgrade, a Pro model (the plan default or the pick) answered: ' + a2.slice(0, 80));
tiers = passTiers();
ok((tiers.pro || 0) >= 1, 'the wallet now holds Pro passes: ' + JSON.stringify(tiers));
shot('upgrade-pro');

// 3. A wiped phone signs in again: the wallet comes back (old passes too); first chat is Pro.
({ page } = await launch(true));
await page.locator('.login input').fill(code);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 60000 });
await agentSwitches(page, false);
await pick(page, FRONTIER, FRONTIER);
const a3 = await ask(page, 'Reply with exactly the word: kiwi');
ok(/kiwi/i.test(a3) && !/plan/i.test(a3), 'signed in again, the frontier model answered: ' + a3.slice(0, 80));
shot('upgrade-restored');
console.log('all upgrade checks passed');
process.exit(0);
