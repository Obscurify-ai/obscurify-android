// The drawer's quick controls, as on the web sidebar: Temp (a chat never written to the phone),
// Tor (the onion link) and Wipe (chats and settings deleted; session and wallet kept).
//   ACCOUNT=<funded number> node e2e/quick.mjs      (test APK built with DEVTOOLS=1)
import { launch, ok, sleep, adb, PKG } from './lib.mjs';

// Needs a rooted emulator (adb root): release builds aren't debuggable, so run-as is refused.
const files = () => adb('shell', 'ls', `/data/user/0/${PKG}/files`).split(/\s+/).filter(Boolean);
const chatFiles = () => files().filter((f) => /^chat-.*\.json$/.test(f));

let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
const say = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 180000 });
  await sleep(500);
};
const drawer = async () => { await page.locator('.icon', { hasText: '☰' }).click(); await page.waitForSelector('.drawer .quick'); };

// The row
await drawer();
ok(await page.locator('.drawer .quick .q').count() === 4 && await page.locator('#encrypted').count() === 1, 'the drawer has Temp, Encrypted, Tor and Wipe');
ok(/\.onion/.test(await page.locator('#tor').getAttribute('href')), 'Tor links the onion site');

// Temp
await page.locator('#temp').click();
ok(await page.locator('#composer').getAttribute('placeholder') === 'Temporary chat', 'the message box says the chat is temporary');
await say('Reply with the single word PELICAN.');
ok(/pelican/i.test(await page.locator('.msg').last().textContent()), 'a temporary chat gets an answer');
ok(chatFiles().length === 0 && !files().includes('chats.json'), 'nothing about it is written to the phone');
await drawer();
ok(await page.locator('#temp').getAttribute('aria-pressed') === 'true', 'Temp shows as on');
ok(await page.locator('.drawer .item').count() === 0, 'it is not in the chat list');

// Off again, and a normal chat is saved
await page.locator('#temp').click();
ok(await page.locator('#composer').getAttribute('placeholder') === 'Message', 'Temp off gives an ordinary chat');
await say('Reply with the single word OTTER.');
ok(chatFiles().length === 1, 'an ordinary chat is saved');

// Wipe, after the app restarts so it reads what's on disk
({ page } = await launch(false));
await drawer();
await page.locator('#wipe').click();
await page.waitForSelector('.wipe-ask');
await page.locator('.wipe-ask button', { hasText: 'Cancel' }).click();
ok(chatFiles().length === 1, 'Cancel keeps the chats');
await page.locator('#wipe').click();
await page.locator('#wipe-yes').click();
await sleep(800);
ok(chatFiles().length === 0 && !files().includes('chats.json') && !files().includes('settings.json'), 'Wipe deletes chats and settings');
ok(files().some((f) => /session/.test(f)), 'the session is kept');
({ page } = await launch(false));
ok(await page.locator('.tools-switch').count() === 1, 'still signed in after a restart');
console.log('all checks passed');
process.exit(0);
