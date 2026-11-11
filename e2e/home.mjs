// Answers keep going while the app is in the background (Home pressed, screen off), plain chat and
// Code Mode, and are there when it comes back.
//   ACCOUNT=<funded number> [MODE=plain|code] [AWAY=90] node e2e/home.mjs   (test APK, DEVTOOLS=1)
import { adb, launch, ok, sleep, PKG, flip, shot } from './lib.mjs';

const MODE = process.env.MODE || 'plain', AWAY = +(process.env.AWAY || 90);
let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill(process.env.MODEL || 'DeepSeek V4 Flash');
await page.locator('.pick').first().locator('.grow').click();
await sleep(1000);
console.log('model:', await page.locator('.title.model').textContent());
const on = (await page.locator('.tools-switch').count()) && (await page.locator('.tools-switch').getAttribute('aria-checked')) === 'true';
if (MODE === 'plain' && on) await flip(page, '.tools-switch');
if (MODE === 'plain' && await page.locator('.code-switch.on').count()) await flip(page, '.code-switch');
const Q = process.env.Q || (MODE === 'plain'
  ? 'Write the numbers from 1 to 400 in English words, one per line, nothing else.'
  : 'Use run_code with Python three separate times, one call per reply step: first print 2**10, then print 3**10, then print 5**10. Then write the numbers from 1 to 150 in English words, one per line, and end with DONE.');
await sleep(400);
await page.locator('#composer').fill(Q);
await sleep(400);
await page.locator('.send').click();
await page.waitForSelector('.send.stop', { timeout: 10000 });
await sleep(4000);
const before = await page.evaluate(() => [...document.querySelectorAll('.msg.assistant')].pop()?.textContent.length || 0);
adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
if (process.env.SWIPE) {
  // Swiped away from Recents: the activity and its WebView go, the process may stay.
  await sleep(1000); adb('shell', 'input', 'keyevent', 'KEYCODE_APP_SWITCH'); await sleep(2000);
  const [w, h] = adb('shell', 'wm', 'size').match(/(\d+)x(\d+)/).slice(1).map(Number);
  adb('shell', 'input', 'swipe', `${w / 2}`, `${h / 2}`, `${w / 2}`, '50', '200'); await sleep(1500);
  adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
  let pid = ''; try { pid = adb('shell', 'pidof', PKG); } catch {}
  console.log('swiped away; process ' + (pid ? 'alive ' + pid : 'gone'));
}
if (process.env.DOZE) {
  // A real phone: unplugged, screen off, deep Doze, and the app in the bucket Android picks.
  adb('shell', 'dumpsys', 'battery', 'unplug');
  if (process.env.BUCKET) adb('shell', 'am', 'set-standby-bucket', PKG, process.env.BUCKET);
  adb('shell', 'input', 'keyevent', 'KEYCODE_SLEEP');
  await sleep(1000);
  console.log(adb('shell', 'dumpsys', 'deviceidle', 'force-idle'));
}
console.log(`home pressed (${before} chars so far), away ${AWAY}s`);
await sleep(AWAY * 1000);
if (process.env.DOZE) { adb('shell', 'dumpsys', 'deviceidle', 'unforce'); adb('shell', 'dumpsys', 'battery', 'reset'); adb('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP'); adb('shell', 'wm', 'dismiss-keyguard'); await sleep(1500); }
const notes = adb('shell', 'dumpsys', 'notification', '--noredact').split('\n').filter((l) => /android.title|android.text/.test(l) && /Obscurify|answer|Writing|Working/i.test(l)).slice(0, 6).join('\n');
console.log('notifications while away:\n' + notes);
if (process.env.SWIPE) ({ page } = await launch(false, true)); else adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`);
await sleep(process.env.SWIPE ? 20000 : 3000);
shot('home-back');
const state = await page.evaluate(() => ({ busy: !!document.querySelector('.send.stop'), text: [...document.querySelectorAll('.msg.assistant')].pop()?.textContent || '', err: [...document.querySelectorAll('.msg.error, .err')].map((e) => e.textContent).join(' | ') }));
console.log('back:', JSON.stringify({ busy: state.busy, len: state.text.length, tail: state.text.slice(-200), err: state.err }));
ok(!state.busy && new RegExp(process.env.END || (MODE === 'plain' ? 'four hundred' : 'DONE'), 'i').test(state.text), 'the answer finished while the app was in the background');
process.exit(0);
