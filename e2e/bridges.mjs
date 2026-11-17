// Tor bridges: a phone that last needed bridges (tor-bridges.json) connects through Tor's built-in
// bridges via the packaged lyrebird, never directly, and chat works over them.
//   node e2e/bridges.mjs      (test APK built with DEVTOOLS=1, Go present at build; a rooted emulator)
import { launch, ok, sleep, adb, PKG } from './lib.mjs';
const dir = `/data/user/0/${PKG}/files`;
let { page } = await launch(true);
adb('shell', `echo true > ${dir}/tor-bridges.json`);
await page.getByRole('button', { name: /guest/i }).first().click();
await page.locator('.icon', { hasText: '☰' }).click();
const t0 = Date.now();
await page.locator('#tor').click();
await page.waitForFunction(() => document.querySelector('#tor')?.classList.contains('on') || document.querySelector('#tor')?.classList.contains('fail'), null, { timeout: 300000, polling: 1000 });
const cls = await page.locator('#tor').getAttribute('class');
console.log('status', cls, ((Date.now() - t0) / 1000).toFixed(1), 's');
console.log(adb('shell', 'ls', `${dir}/tor`));
console.log(adb('shell', 'ps -A | grep -i lyrebird || true'));
ok(/on/.test(cls), 'bridges: Tor connected');
ok(!adb('shell', 'ls', `${dir}/tor`).includes('direct'), 'it connected through the bridges, never directly');
await page.locator('.veil').click({ position: { x: 380, y: 300 } });
await page.locator('#composer').fill('Reply with the single word WREN.');
await page.locator('.send').click();
await page.waitForSelector('.send.stop', { timeout: 15000 }).catch(() => {});
await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 240000 });
ok(/wren/i.test(await page.locator('.msg').last().textContent()), 'bridges: a chat is answered');
process.exit(0);
