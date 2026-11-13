// Encrypted mode: end-to-end encrypted chat with TEE models, checked on the phone. The quick control
// (where Verify was) turns it on and picks a model that takes encrypted requests; the picker lists
// only those; a marker word sent in Encrypted mode never appears in anything the phone sends (the
// proxy sees every request), while the decrypted answer shows it; the answer's chip says
// "Encrypted" once all eight checks pass, including that the enclave received exactly what was
// encrypted; a second turn recalls the marker (the whole history went, encrypted); a server that
// swaps the enclave's key gets "Not encrypted: ..." and nothing; a reply with one byte changed says
// "Check failed", on the reply-bytes check;
// an answer the app was killed in the middle of is read with the key it kept; results stay with the
// chat. Real models, through flaky-proxy.mjs to a server with Encrypted mode:
//   TARGET=http://127.0.0.1:8251 node e2e/flaky-proxy.mjs &   (test APK with OBSCURIFY_API=http://127.0.0.1:8093,
//   adb reverse tcp:8093 tcp:8093)
//   adb root; ACCOUNT=<funded number> node e2e/encrypted.mjs
import { execFileSync } from 'node:child_process';
import { randomInt } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { launch, ok, sleep, PKG } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const SHOTS = process.env.SHOTS || `${process.env.HOME}/Work/obscurify/screenshots`;
const shot = (name) => execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/${name}.png`]);
const BYTES = 'The reply you received is the one the gateway signed';
const REQUEST = 'The enclave received exactly the conversation you sent';
// Easy for a model to copy exactly, and nowhere else.
const MARKER = 'Orchid' + randomInt(100, 999);
const leaks = async () => JSON.parse(await ctl('__grep?q=' + MARKER));

let { page } = await launch(true);
await ctl('__set');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
const idle = () => page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 280000 });
const type = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
};
const say = async (text) => { await type(text); await idle(); await sleep(300); };
const last = () => page.locator('.msg.assistant').last();
const lastChip = () => last().locator('.tee-chip');
const settled = () => page.waitForFunction(() => {
  const c = [...document.querySelectorAll('.msg.assistant')].pop()?.querySelector('.tee-chip');
  return c && !c.classList.contains('wait');
}, null, { timeout: 180000 });
const rowsOfLast = async () => {
  if ((await lastChip().getAttribute('aria-expanded')) !== 'true') await lastChip().click();
  return last().locator('.tee-row').evaluateAll((rs) => rs.map((r) => ({ cls: r.className.replace('tee-row', '').trim(), text: r.textContent })));
};

// 1. The control: renamed, where Verify was; it picks a model that takes encrypted requests.
await page.locator('.bar .icon').first().click();
ok((await page.locator('#encrypted').textContent()).trim() === 'Encrypted', 'the quick control says Encrypted');
ok(!(await page.locator('#verify').count()), 'there is no Verify control any more');
await page.locator('#encrypted').click();
await page.waitForSelector('.title.model .verify-mark', { timeout: 5000 });
const title = await page.locator('.title.model').textContent();
ok(title.includes('GLM 5.3'), 'Encrypted picks GLM 5.3: ' + title);
await page.locator('.bar .icon').first().click();
ok((await page.locator('#encrypted').getAttribute('aria-pressed')) === 'true', 'Encrypted is pressed');
await page.locator('.veil').click({ position: { x: 380, y: 400 } }).catch(() => {});
await sleep(300);
const catalogue = (await (await fetch('http://127.0.0.1:8093/v1/models')).json()).data;
const e2ee = catalogue.filter((m) => m.e2ee).map((m) => m.name);
ok(catalogue.filter((m) => m.id.startsWith('tee/')).every((m) => m.e2ee), 'every tee/ model takes encrypted requests');
await page.locator('.title.model').click();
await page.locator('.chip', { hasText: /^All$/ }).click();
const names = await page.locator('.pick .grow > div:first-child').allTextContents();
ok(names.length > 0 && names.every((n) => e2ee.includes(n.trim())), `the picker lists only encrypted models (${names.length}): ${names.join(', ')}`);
await page.locator('.pick', { hasText: 'GLM 5.3 Flash TEE' }).first().locator('.grow').click();
ok((await page.locator('.tools-switch').getAttribute('aria-checked')) !== 'true', 'tools are off');

// 2. A message with the marker: only ciphertext leaves the phone, the answer comes back decrypted.
await say(`The password is ${MARKER}. Reply with the password and nothing else.`);
await settled();
ok((await last().textContent()).includes(MARKER), 'the decrypted answer has the marker');
ok((await lastChip().textContent()).trim() === 'Encrypted', 'the answer says Encrypted: ' + await lastChip().textContent());
let rows = await rowsOfLast();
ok(rows.length === 8 && !rows.some((r) => r.cls.includes('fail')), 'eight checks, none failed: ' + rows.map((r) => r.cls).join(' '));
ok(rows.some((r) => r.text.includes(REQUEST) && r.cls.includes('pass')), 'the enclave received exactly what was encrypted');
const sent = JSON.parse(await ctl('__log')).pop();
ok(sent.messages.every((m) => /^04[0-9a-f]{184,}$/.test(m.content) && Object.keys(m).join() === 'role,content'), 'every message sent is ciphertext');
ok(!('thread' in sent) && !('tools' in sent) && !('web_search' in sent) && sent.stream_options?.include_usage === true, 'no thread, tools or web search; usage asked for');
ok((await leaks()).length === 0, 'the marker never left the phone');
await sleep(500);
shot('android-encrypted-checks');

// 3. A second turn: the whole history goes again, all encrypted.
await say('What was the password? Reply with it only.');
await settled();
ok((await last().textContent()).includes(MARKER), 'the second answer recalls the marker');
ok((await lastChip().textContent()).trim() === 'Encrypted', 'the second answer says Encrypted');
ok(JSON.parse(await ctl('__log')).pop().messages.length >= 3, 'the second request carries the history');
ok((await leaks()).length === 0, 'the marker still never left the phone');

// 4. A server that swaps the enclave's key for its own: nothing is sent.
await ctl('__set?swapkey=1');
await say(`Never send this: ${MARKER}`);
const refusal = (await page.locator('.msg.err').last().textContent().catch(() => '')) || '';
ok(refusal.startsWith("Not encrypted: the enclave's attestation didn't verify, so nothing was sent."), 'a swapped key is refused: ' + refusal.slice(0, 200));
ok(JSON.parse(await ctl('__log')).length === 0, 'no chat request was made');
shot('android-encrypted-refused');

// 5. One byte changed on the way: Check failed, on the reply-bytes check.
await ctl('__set?teeflip=1');
await say('Reply with exactly one word: fine');
await settled();
ok(await lastChip().evaluate((e) => e.classList.contains('bad')) && (await lastChip().textContent()).trim() === 'Check failed', 'a changed reply says Check failed: ' + await lastChip().textContent());
const why = await last().locator('.tee-why').textContent();
ok(why.includes(BYTES), 'and it says which check failed: ' + why);
rows = await rowsOfLast();
ok(rows[0].cls.includes('fail') && rows.filter((r) => r.cls.includes('fail')).length === 1, 'only the reply-bytes check failed');
await ctl('__set');

// 6. Killed mid-answer: opened again, the rest is read with the key the app kept.
await type('Write the numbers from one to sixty in words, one per line, nothing else.');
await page.waitForFunction(() => /three/i.test([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || ''), null, { timeout: 120000 });
({ page } = await launch(false));
await page.waitForFunction(() => /sixty/i.test([...document.querySelectorAll('.msg.assistant')].pop()?.textContent || '') && !document.querySelector('.send.stop'), null, { timeout: 280000 });
await settled();
ok(/fifty-nine|fifty nine/i.test(await last().textContent()), 'the picked-up answer is whole and decrypted');
ok((await lastChip().textContent()).trim() === 'Encrypted', 'the picked-up answer says Encrypted: ' + await lastChip().textContent());
shot('android-encrypted-chip');

// 7. The results stay with the chat.
({ page } = await launch(false));
await page.waitForSelector('.tee-chip', { timeout: 20000 });
const kept = await page.locator('.tee-chip').evaluateAll((cs) => cs.map((c) => c.className + ' ' + c.textContent.trim()));
ok(kept.length === 4 && kept.filter((c) => c.endsWith(' ok Encrypted')).length === 3 && kept.filter((c) => c.endsWith(' bad Check failed')).length === 1, 'the results are kept after a restart: ' + kept.join(' | '));
ok((await leaks()).length === 0, 'the marker never left the phone at all');
const verifyAsks = JSON.parse(await ctl('__grep?q=tee_verify'));
ok(verifyAsks.length === 0, 'no request ever asked for tee_verify: ' + JSON.stringify(verifyAsks));

// 7b. A check saved before Encrypted mode (an answer that was never encrypted) is never called
// Encrypted: it still shows as it was saved.
// Needs `adb root`: the chat's file loses its first answer's encrypted flag.
const dir = `/data/user/0/${PKG}/files`;
const file = execFileSync('adb', ['shell', `grep -l '"encrypted":true' ${dir}/chat-*.json`], { encoding: 'utf8' }).trim().split(/\s+/)[0];
const json = execFileSync('adb', ['shell', 'cat', file], { encoding: 'utf8', maxBuffer: 1 << 26 });
writeFileSync('/tmp/obs-e2e-chat.json', json.replace(',"encrypted":true', ''));
const owner = execFileSync('adb', ['shell', 'stat', '-c', '%u:%g', file], { encoding: 'utf8' }).trim();
execFileSync('adb', ['push', '/tmp/obs-e2e-chat.json', file]);
execFileSync('adb', ['shell', 'chown', owner, file]);
({ page } = await launch(false));
await page.waitForSelector('.tee-chip', { timeout: 20000 });
const legacy = await page.locator('.tee-chip').first().textContent();
ok(legacy.trim() === 'Verified', 'an old Verify-mode answer still says Verified: ' + legacy);

// 8. Encrypted off: plain chat again.
await page.locator('.bar .icon').first().click();
await page.locator('#encrypted').click();
await sleep(500);
ok(!(await page.locator('.title.model .verify-mark').count()), 'Encrypted off');
console.log('encrypted: all passed');
process.exit(0);
