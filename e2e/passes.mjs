// Private chats prove what the account's plan gives with passes that carry a label for what a
// request needs (frontier, uncensored, free), never the plan (unlinkable-inference 0.2). A chat
// shows one only when the server asks, so on an open model a Pro chat and a Pay As You Go chat
// look the same to the server.
//
// - INPLACE=1 first: the app was installed over 0.8.51 (wallet full of plan passes, signed in on
//   Pro): it starts, and a frontier model answers.
// - Pay As You Go: a chat works, the wallet holds only free-model passes, and the server's row
//   for the chat's session shows nothing. The guest model takes a free pass and costs nothing.
// - Upgrade to Pro (in the DB, as billing does), restart the app: an uncensored model and a
//   frontier model answer, each session showing just that pass. An open model: nothing shown.
// - Wipe the app and sign in again (the wallet comes back from the code): a frontier model
//   answers on the first chat.
//
//   OBSCURIFY_API=http://127.0.0.1:8231 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8231 tcp:8230; DB=obscurify_upgrade node e2e/passes.mjs
// Needs the dev Postgres container and a dev server with private sessions on. Spends a few cents.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG, wordsOn } from './lib.mjs';

const DB = process.env.DB || 'obscurify_upgrade';
const FRONTIER = process.env.FRONTIER || 'Claude Haiku 5.5';
const UNCENSORED = process.env.UNCENSORED || 'DeepSeek V4.1 Flash Uncensored';
// An open model, by its exact name: other names contain it (abliterated variants).
const OPEN = process.env.OPEN || 'DeepSeek: DeepSeek V4.1 Flash';
// The server's guest model (DEFAULT_FREE_MODEL): free within the plan's free-model tokens.
const GUEST = process.env.GUEST || 'Meta: Llama 3.3 70b Instruct';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const files = `/data/user/0/${PKG}/files`;
const state = () => { try { return JSON.parse(adb('shell', 'cat', `${files}/unlinkable-state.json`)); } catch { return {}; } };
// The wallet's passes by label (wallets from before 0.2 say `tier`).
const passLabels = () => {
  const n = {};
  for (const p of (state().wallet || {}).passes || []) n[p.label || p.tier] = (n[p.label || p.tier] || 0) + 1;
  return n;
};
// What the server holds about the chat used last: its session row, minus keys, times and money.
const lastSession = () => {
  const chats = Object.values(state().chats || {}).sort((a, b) => b[1] - a[1]);
  if (!chats.length) return 'no session';
  const hash = createHash('sha256').update(chats[0][0]).digest('hex');
  return sql(`SELECT 'plan=' || COALESCE(s.pass_tier, '-') || ' labels=' || array_to_string(u.session_labels, '+') || ' free=' || s.free_token_allowance || ' tier=' || u.account_tier FROM unlinkable_sessions s JOIN users u ON u.id = s.user_id WHERE s.secret_hash = '${hash}'`);
};

const pick = async (page, search, name) => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(search);
  await page.locator('.pick', { hasText: name }).filter({ hasNotText: 'Thinking' }).first().locator('.grow').click();
};
// The model with exactly this name.
const pickExact = async (page, name) => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(name.split(': ').pop());
  await page.locator('.pick').filter({ has: page.getByText(name, { exact: true }) }).first().locator('.grow').click();
};
const ask = async (page, text, timeout = 240000) => {
  const before = await page.locator('.msg.assistant, .msg.err').count();
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), before, { timeout });
  await sleep(1500); // the wallet's state is saved after the request
  return (await page.locator('.msg.assistant, .msg.err').last().textContent()).trim();
};
const newChat = async (page) => { await page.locator('.bar .icon', { hasText: '＋' }).click(); await sleep(500); };
let page;

// 0. Updated in place from 0.8.51: plan passes in the wallet, signed in on Pro.
if (process.env.INPLACE) {
  const before = passLabels();
  ok((before.pro || 0) > 0, 'the wallet from the old app holds plan passes: ' + JSON.stringify(before));
  ({ page } = await launch(false));
  await page.waitForSelector('#composer', { timeout: 60000 });
  await agentSwitches(page, false);
  await newChat(page);
  await pick(page, FRONTIER, FRONTIER);
  const a0 = await ask(page, 'Reply with exactly the word: plum');
  ok(/plum/i.test(a0) && a0.includes(FRONTIER) && !/plan/i.test(a0), 'updated over the old app: the frontier model answered: ' + a0.slice(0, 80));
  const after = passLabels();
  ok(after.frontier > 0 && after.uncensored > 0 && after.free > 0 && after.pro === before.pro, 'the wallet kept its old passes and stocked the new ones: ' + JSON.stringify(after));
  ok(/^plan=- labels=frontier free=0/.test(lastSession()), 'its session showed the frontier pass, not the plan: ' + lastSession());
  shot('passes-inplace');
}

// 1. Sign up; fund the account as plain Pay As You Go (no starter Pro trial).
({ page } = await launch(true));
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
await pickExact(page, OPEN);
const a1 = await ask(page, 'Reply with exactly the word: pineapple');
ok(/pineapple/i.test(a1) && a1.includes(OPEN), 'Pay As You Go chat answered: ' + a1.slice(0, 80));
let labels = {};
for (let i = 0; i < 20 && !labels.free; i++) { await sleep(1000); labels = passLabels(); }
ok(labels.free === 20 && Object.keys(labels).length === 1, 'the Pay As You Go wallet holds only free-model passes: ' + JSON.stringify(labels));
const paygRow = lastSession();
ok(paygRow === 'plan=- labels= free=0 tier=pay_as_you_go', 'its session shows nothing: ' + paygRow);
await newChat(page);
await pickExact(page, GUEST);
const a1b = await ask(page, 'Reply with exactly the word: papaya');
ok(/papaya/i.test(a1b) && a1b.includes(GUEST), 'the guest model answered: ' + a1b.slice(0, 80));
ok(lastSession() === 'plan=- labels= free=50000 tier=pay_as_you_go', 'its session showed a free pass: ' + lastSession());
const cost = sql(`SELECT coverage || ' ' || amount::float FROM invoices WHERE lang_model LIKE '%llama-3.3-70b-instruct' ORDER BY created_at DESC LIMIT 1`);
ok(cost === 'free_model 0', 'the guest model cost nothing: ' + cost);
shot('passes-payg');

// 2. Upgrade to Pro, restart the app: the Pro models answer, each chat showing only its pass.
sql(`UPDATE users SET account_tier = 'pro', subscription_started_at = NOW(), last_subscription_charge = NOW() WHERE id = '${userId}'`);
({ page } = await launch(false));
await newChat(page);
await pick(page, UNCENSORED, UNCENSORED);
const a2 = await ask(page, 'Reply with exactly the word: mango');
ok(/mango/i.test(a2) && a2.includes(UNCENSORED) && !/plan/i.test(a2), 'after the upgrade, the uncensored model answered: ' + a2.slice(0, 80));
ok(/^plan=- labels=uncensored free=0/.test(lastSession()), 'its session showed the uncensored pass only: ' + lastSession());
await newChat(page);
await pick(page, FRONTIER, FRONTIER);
const a2b = await ask(page, 'Reply with exactly the word: lemon');
ok(/lemon/i.test(a2b) && a2b.includes(FRONTIER) && !/plan/i.test(a2b), 'a frontier model answered: ' + a2b.slice(0, 80));
ok(/^plan=- labels=frontier free=0/.test(lastSession()), 'its session showed the frontier pass only: ' + lastSession());
await newChat(page);
await pickExact(page, OPEN);
const a2c = await ask(page, 'Reply with exactly the word: cherry');
ok(/cherry/i.test(a2c) && a2c.includes(OPEN), 'Pro, open model: answered: ' + a2c.slice(0, 80));
ok(lastSession() === paygRow, 'on an open model the Pro chat is the same row as the Pay As You Go one: ' + lastSession());
labels = passLabels();
ok(labels.frontier > 0 && labels.uncensored > 0 && labels.free > 0 && Object.keys(labels).length === 3, 'the wallet holds passes of each label: ' + JSON.stringify(labels));
shot('passes-pro');

// 3. A wiped phone signs in again: the wallet comes back; the first chat has the Pro models.
({ page } = await launch(true));
await page.locator('.login input').fill(code);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 60000 });
await agentSwitches(page, false);
await pick(page, FRONTIER, FRONTIER);
const a3 = await ask(page, 'Reply with exactly the word: kiwi');
ok(/kiwi/i.test(a3) && a3.includes(FRONTIER) && !/plan/i.test(a3), 'signed in again, the frontier model answered: ' + a3.slice(0, 80));
shot('passes-restored');
console.log('all pass checks passed');
process.exit(0);
