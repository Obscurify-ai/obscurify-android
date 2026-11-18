// A phone signed in before private sessions (no wallet seed) is asked for the account number once in
// the chat, with a "How this works" link: a wrong number says so and changes nothing; the right one
// turns private sessions on for the same account; "Not now" is remembered. A phone that signed up
// here, or signed in since, is never asked (GET /private_setup).
//   OBSCURIFY_API=http://127.0.0.1:8231 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8231 tcp:8230; DB=obscurify_unlinkable node e2e/private-setup.mjs
import { execFileSync } from 'node:child_process';
import { launch, adb, ok, shot, sleep, PKG, wordsOn } from './lib.mjs';

const DB = process.env.DB || 'obscurify_unlinkable';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();

const files = `/data/user/0/${PKG}/files`;
const has = (f) => { try { adb('shell', 'ls', `${files}/${f}`); return true; } catch { return false; } };
const sheet = (page) => page.locator('.sheet.private-setup');

// 1. Sign up here: never asked.
let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
const number = await wordsOn(page, '.number', 30000);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
await sleep(1500);
ok(await sheet(page).count() === 0, 'signed up on this phone: not asked');
const userId = sql(`SELECT id FROM users ORDER BY created_at DESC LIMIT 1`);

// 2. No seed on the phone; `old`: its sign-in dates from before private sessions.
const asOldPhone = async (old = true) => {
  adb('shell', 'am', 'force-stop', PKG);
  adb('shell', 'rm', '-f', `${files}/unlinkable-seed.json`, `${files}/unlinkable-state.json`, `${files}/private-setup-later.json`);
  sql(`UPDATE sessions SET created_at = ${old ? "'2026-11-01'" : 'NOW()'} WHERE user_id = '${userId}'`);
  ({ page } = await launch(false));
  await page.waitForSelector('#composer', { timeout: 20000 });
};
await asOldPhone(false);
await sleep(4000);
ok(await sheet(page).count() === 0, 'a sign-in since private sessions, without the seed: not asked');
await asOldPhone();
await sheet(page).waitFor({ timeout: 15000 });
ok(true, 'a sign-in from before private sessions: asked');
ok(await sheet(page).getByText('How this works').count() === 1, 'with a "How this works" link');
shot('private-setup');

await sheet(page).locator('input').fill('1111 2222 3333 4444');
await sheet(page).getByRole('button', { name: 'Turn on' }).click();
await sheet(page).locator('.err').waitFor({ timeout: 30000 });
ok(/did not sign in/.test(await sheet(page).locator('.err').textContent()), 'a wrong number says so');
ok(!has('unlinkable-seed.json'), 'and leaves no wallet key behind');

await sheet(page).locator('input').fill(number);
await sheet(page).getByRole('button', { name: 'Turn on' }).click();
await sheet(page).waitFor({ state: 'detached', timeout: 60000 });
ok(has('unlinkable-seed.json'), 'the right number: private sessions on (the wallet key is on the phone)');
await page.locator('.bar .icon').first().click();
await page.getByText('Account', { exact: false }).last().click();
await page.waitForFunction(() => /\$\d/.test(document.querySelector('.section .big')?.textContent || ''), null, { timeout: 30000 });
ok(true, 'still signed in, balance ' + await page.locator('.section .big').first().textContent());

// 3. "Not now" is remembered.
await asOldPhone();
await sheet(page).waitFor({ timeout: 10000 });
await sheet(page).getByRole('button', { name: 'Not now' }).click();
ok(await sheet(page).count() === 0, '"Not now" closes it');
({ page } = await launch(false));
await page.waitForSelector('#composer', { timeout: 20000 });
await sleep(1500);
ok(await sheet(page).count() === 0 && !has('unlinkable-seed.json'), '"Not now" is remembered; private sessions stay off');
console.log('all private-setup checks passed');
process.exit(0);
