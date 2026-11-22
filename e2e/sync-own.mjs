// What the phone just sent coming back to it is not a change from another device: a chat deleted
// right after it synced stays deleted, and an edit made right after stays made (src/sync.rs, apply).
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; REV=8232:<server port> DB=<dev database> node e2e/sync-own.mjs
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, sleep, agentSwitches, PKG } from './lib.mjs';
const [app, host] = (process.env.REV || '8232:8097').split(':');
adb('reverse', `tcp:${app}`, `tcp:${host}`);
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', process.env.DB || 'obscurify_sync', '-tAX', '-c', q], { encoding: 'utf8' }).trim();
const files = `/data/user/0/${PKG}/files`;
const json = (n, f) => { try { return JSON.parse(execFileSync('adb', ['shell', 'cat', `${files}/${n}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] })); } catch { return f; } };
const { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
await page.locator('.number').waitFor({ timeout: 40000 });
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
const id = sql("SELECT id FROM users WHERE NOT is_session ORDER BY created_at DESC LIMIT 1");
sql(`UPDATE users SET balance = balance + 5 WHERE id = '${id}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${id}', 5, 'deposit')`);
await page.locator('.title.model').click();
await page.locator('input.search').fill('Claude Haiku');
await page.locator('.drawer .pick:not(.locked)').first().locator('.grow').click();
await agentSwitches(page, false);
const ask = async (t) => {
  const n = await page.locator('.msg.assistant, .msg.err').count();
  await page.locator('#composer').fill(t); await sleep(300); await page.locator('.send').click();
  await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), n, { timeout: 120000 });
};
const fresh = async () => { await page.locator('.bar .icon', { hasText: '＋' }).click().catch(() => {}); await sleep(700); };
const find = (w) => json('chats.json', []).map((m) => json(`chat-${m.id}.json`, {})).filter((c) => JSON.stringify(c.messages || []).includes(w));
const N = Number(process.env.N || 3);
let back = 0, undone = 0;
for (let i = 0; i < N; i++) {
  await fresh();
  await ask(`Reply with exactly: plum${i}`);
  await sleep(4000); // the answer has gone up
  await page.locator('.icon', { hasText: '☰' }).click();
  const row = page.locator('.drawer .item', { hasText: `plum${i}` });
  await row.locator('button.bare').click(); await row.locator('button.danger').click();
  await sleep(500);
  const gone = find(`plum${i}`).length === 0;
  await sleep(7000);
  const again = find(`plum${i}`).length;
  if (again) back++;
  console.log(i, 'deleted', gone, 'after sync', again);
  await page.locator('.veil').first().click({ position: { x: 380, y: 400 } }).catch(() => {});
  await fresh();
  await ask(`Reply with exactly: fig${i}`);
  await ask(`Reply with exactly: date${i}`);
  await sleep(4000);
  await page.getByRole('button', { name: 'Edit', exact: true }).last().click();
  await sleep(600);
  const cut = find(`fig${i}`).map((c) => c.messages.length).join();
  await sleep(7000);
  const after = find(`fig${i}`).map((c) => c.messages.length).join();
  if (after !== '2') undone++;
  console.log(i, 'edit left', cut, 'after sync', after);
  await page.locator('#composer').fill('');
}
ok(back === 0, `${N} chats deleted right after they synced stay deleted`);
ok(undone === 0, `${N} edits made right after a chat synced stay made`);
process.exit(0);
