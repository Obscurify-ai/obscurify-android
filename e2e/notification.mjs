// The notifications carry the Obscurify logo: a guest's answer, sent to the background, is shown in the
// status bar and the shade while it is worked on and again when it is ready. Leaves screenshots to look at.
//   [SHOTS=/tmp] node e2e/notification.mjs   (test APK, DEVTOOLS=1)
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { adb, launch, ok, sleep, PKG } from './lib.mjs';

const SHOTS = process.env.SHOTS || '/tmp';
const shot = name => execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/notification-${name}.png`]);
const icons = () => adb('shell', 'dumpsys', 'notification', '--noredact').split('NotificationRecord(')
  .filter(r => r.includes(`pkg=${PKG} `)).map(r => ({ channel: r.match(/channel=(\w+)/)?.[1], icon: r.match(/icon=Icon\(([^)]*)\)/)?.[1] }));

const src = readFileSync(new URL('../android-res/drawable/ic_work.xml', import.meta.url), 'utf8');
const logo = readFileSync(new URL('../android-res/drawable-v24/ic_launcher_foreground.xml', import.meta.url), 'utf8');
const mark = s => s.match(/android:pathData="([^"]+)"/)[1];
ok(mark(src) === mark(logo), 'the notification icon is the same mark as the app icon');

const { page } = await launch(true);
await page.getByRole('button', { name: 'Chat as a guest' }).click();
await page.locator('#composer').fill('Write the numbers from 1 to 300 in English words, one per line, nothing else.');
await sleep(400);
await page.locator('.send').click();
await page.waitForSelector('.send.stop', { timeout: 15000 });
await sleep(1500);
adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
// Android holds a foreground service's notification back for up to 10 seconds.
for (let i = 0; i < 20 && !icons().some(n => n.channel === 'work'); i++) await sleep(1000);
console.log(icons());
ok(icons().some(n => n.channel === 'work'), 'a notification shows while the answer is worked on');
shot('bar');
adb('shell', 'cmd', 'statusbar', 'expand-notifications');
await sleep(2000);
shot('working');
for (let i = 0; i < 90 && !icons().some(n => n.channel === 'done'); i++) await sleep(2000);
console.log(icons());
ok(icons().some(n => n.channel === 'done'), 'a notification says the answer is ready');
await sleep(3000); // the shade animates the new one in
shot('done');
adb('shell', 'cmd', 'statusbar', 'collapse');
process.exit(0);
