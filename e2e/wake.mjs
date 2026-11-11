// A connection that died without saying so (the phone slept; a VPN or Wi-Fi dropped it) doesn't
// leave the answer stuck: back on screen, the app asks for the rest at once and the answer finishes.
//   node flaky-proxy.mjs &   (TARGET defaults to https://obscurify.ai; test APK with DEVTOOLS=1
//   OBSCURIFY_API=http://127.0.0.1:8093, adb reverse tcp:8093 tcp:8093)
//   ACCOUNT=<funded number> node e2e/wake.mjs
import { adb, launch, ok, sleep, PKG, flip } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const { page } = await launch(true);
await ctl('__set');
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
if ((await page.locator('.tools-switch').getAttribute('aria-checked')) === 'true') await flip(page, '.tools-switch');
if (await page.locator('.code-switch.on').count()) await flip(page, '.code-switch');

await ctl('__set?fail=1&kind=freeze');
await sleep(400);
await page.locator('#composer').fill('Count from 1 to 40 in English words, one per line, nothing else.');
await sleep(400);
await page.locator('.send').click();
await page.waitForSelector('.send.stop', { timeout: 10000 });
// The answer starts, then nothing: the phone goes to sleep with the connection silently dead.
await sleep(5000);
adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
await sleep(25000);
const back = Date.now();
adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`);
await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 60000 });
const took = (Date.now() - back) / 1000;
const a = await page.locator('.msg.assistant').last().textContent();
ok(/forty/i.test(a) && !/dropped|error/i.test(a), 'the answer arrives whole after the dead connection');
ok(took < 12, `back on screen, it was picked up at once (${took.toFixed(1)} s, not the 45 s stall)`);
process.exit(0);
