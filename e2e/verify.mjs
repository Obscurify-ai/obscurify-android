// Verify mode: TEE answers checked on the phone. With Verify on, the picker lists only models whose
// replies can be checked; each answer gets a chip that turns Verified once its receipt, the
// enclave's attestation (with the phone's own nonce), Intel's collateral and the gateway's release
// check out; a reply with one byte changed on the way is Not verified on the reply-bytes check; a
// Code Mode answer is checked step by step; the result stays with the chat. Real models, through
// flaky-proxy.mjs to a server:
//   TARGET=http://127.0.0.1:8241 node flaky-proxy.mjs &   (test APK with OBSCURIFY_API=http://127.0.0.1:8093,
//   adb reverse tcp:8093 tcp:8093)
//   ACCOUNT=<funded number> node e2e/verify.mjs
import { execFileSync } from 'node:child_process';
import { launch, ok, sleep } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const SHOTS = process.env.SHOTS || `${process.env.HOME}/Work/obscurify/screenshots`;
const shot = (name) => execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/${name}.png`]);
const BYTES = 'The reply you received is the one the gateway signed';

let { page } = await launch(true);
await ctl('__set');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { timeout: 30000 });
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const say = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await idle();
  await sleep(300);
};
const lastChip = () => page.locator('.msg.assistant').last().locator('.tee-chip');
const settled = () => page.waitForFunction(() => {
  const c = [...document.querySelectorAll('.msg.assistant')].pop()?.querySelector('.tee-chip');
  return c && !c.classList.contains('wait');
}, null, { timeout: 180000 });

// 1. Verify on: a verifiable model is picked and the picker shows only those.
await page.locator('.bar .icon').first().click();
await page.locator('#verify').click();
await page.waitForSelector('.title.model .verify-mark', { timeout: 5000 });
ok((await page.locator('.title.model').textContent()).includes('TEE'), 'Verify picks a verifiable TEE model: ' + await page.locator('.title.model').textContent());
await page.locator('.title.model').click();
await page.locator('.chip', { hasText: /^All$/ }).click();
const names = await page.locator('.pick .grow > div:first-child').allTextContents();
ok(names.length > 0 && names.every((n) => n.includes('TEE')), `the picker lists only verifiable models (${names.length}): ${names.join(', ')}`);
await page.locator('.pick', { hasText: 'GLM 5.3 Flash TEE' }).first().locator('.grow').click();

// 2. A plain answer (Tools off) is checked: all seven checks.
if ((await page.locator('.tools-switch').getAttribute('aria-checked')) === 'true') await page.locator('.tools-switch').click();
await say('Reply with exactly one word: ok');
await settled();
ok(await lastChip().evaluate((e) => e.classList.contains('ok')), 'the answer is Verified: ' + await lastChip().textContent());
await lastChip().click();
const rows = page.locator('.msg.assistant').last().locator('.tee-row');
const statuses = await rows.evaluateAll((rs) => rs.map((r) => r.className.replace('tee-row', '').trim()));
ok(statuses.length === 7 && !statuses.includes('fail') && statuses.filter((s) => s === 'pass').length >= 6, 'seven checks, none failed: ' + statuses.join(' '));
ok((await page.locator('.msg.assistant').last().textContent()).includes('Receipt rcpt-'), 'the receipt id is shown');
await sleep(500);
shot('android-verify-checks');

// 3. One byte changed on the way: Not verified, on the reply-bytes check.
await ctl('__set?teeflip=1');
await say('Reply with exactly one word: fine');
await settled();
ok(await lastChip().evaluate((e) => e.classList.contains('bad')), 'a changed reply is Not verified');
const why = await page.locator('.msg.assistant').last().locator('.tee-why').textContent();
ok(why.includes(BYTES), 'and it says which check failed: ' + why);
await lastChip().click();
const first = await page.locator('.msg.assistant').last().locator('.tee-row').first().getAttribute('class');
ok(first.includes('fail'), 'the reply-bytes check is the failed one');
shot('android-verify-failed');
await ctl('__set');

// 4. Code Mode: every step is checked.
await page.locator('.tools-switch').click();
await say('Use run_code with Python to print 6*7, then reply with the number only.');
await settled();
const chip = await lastChip().textContent();
ok(await lastChip().evaluate((e) => e.classList.contains('ok')) && /\d+ steps/.test(chip), 'a Code Mode answer is Verified across its steps: ' + chip);
ok((await page.locator('.msg.assistant').last().textContent()).includes('42'), 'and it ran the code');
shot('android-verify-chip');

// 5. The results stay with the chat.
({ page } = await launch(false));
await page.waitForSelector('.tee-chip', { timeout: 20000 });
const kept = await page.locator('.tee-chip').evaluateAll((cs) => cs.map((c) => c.className));
ok(kept.length === 3 && kept.filter((c) => c.includes('ok')).length === 2 && kept.filter((c) => c.includes('bad')).length === 1, 'the results are kept after a restart: ' + kept.join(' | '));

// 6. Verify off: the picker lists everything again.
await page.locator('.bar .icon').first().click();
await page.locator('#verify').click();
await sleep(500);
ok(!(await page.locator('.title.model .verify-mark').count()), 'Verify off');
console.log('verify: all passed');
process.exit(0);
