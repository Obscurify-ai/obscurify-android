// Named API keys on the phone's Account screen (main.rs NamedKeys): New API key shows the key
// once, it works, Revoke stops that key and leaves the Default key working.
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8232 tcp:8232
//   UPSTREAM_PORT=8097 node api-keys.mjs
import http from 'node:http';
import { launch, adb, ok, shot, sleep, wordsOn, CODE } from './lib.mjs';
import { accountKey } from '../../obscurify-cli/lib/account.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const proxy = http.createServer((req, res) => {
  const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => {
    res.writeHead(r.statusCode, r.headers);
    r.pipe(res);
  });
  up.on('error', () => res.destroy());
  req.pipe(up);
});
await new Promise((r) => proxy.listen(8232, '127.0.0.1', r));
adb('reverse', 'tcp:8232', 'tcp:8232');
const works = async (key) => (await fetch(WEB + '/get_user_balance', { headers: { Authorization: `Bearer ${key}` } })).ok;

try {
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Create Account' }).click();
  const code = await wordsOn(page, '.number');
  ok(CODE.test(code), 'signed up: ' + code);
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  const def = (await (await fetch(WEB + '/generate_token', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ account_key: accountKey(code) }) })).json()).token;

  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#api-keys').scrollIntoViewIfNeeded();
  ok(await page.locator('#new-key').isDisabled(), 'New API key waits for a name');

  await page.locator('#new-key-name').fill('Phone test');
  await page.locator('#new-key').click();
  await page.locator('#new-api-key').waitFor({ timeout: 20000 });
  const key = (await page.locator('#new-api-key .mono').textContent()).trim();
  ok(/^sk-obs1_/.test(key) && key !== def, 'New API key shows a new key');
  ok(/isn.t shown again/.test(await page.locator('#new-api-key p').textContent()), '  once');
  ok(await works(key), '  and it works');
  await page.locator('#api-keys').scrollIntoViewIfNeeded();
  shot('api-keys-new');
  await page.locator('#new-api-key').getByText('Done').click();
  await page.locator('.api-key').first().waitFor();
  ok((await page.locator('.api-key-name').first().textContent()) === 'Phone test', 'it is listed by name');
  ok(!(await page.locator('#api-keys').textContent()).includes(key.slice(14)), '  masked');

  await page.locator('.api-key').getByText('Revoke…').click();
  await page.locator('.api-key').getByText('Revoke key').click();
  await page.waitForFunction(() => !document.querySelector('.api-key'), null, { timeout: 20000 });
  ok(!(await works(key)), 'Revoke stops that key');
  ok(await works(def), '  and the Default key still works');
  console.log('\nall API key checks passed');
} catch (e) {
  console.error(e.message);
  process.exitCode = 1;
} finally {
  proxy.close();
  adb('reverse', '--remove', 'tcp:8232');
  // The device connection keeps Node running otherwise.
  process.exit(process.exitCode || 0);
}
