// Mirror failover (Pro): signed in, the app keeps the account's mirrors (GET /api/mirrors). When the
// server can't be reached at all, a request moves to a mirror that answers and carries on there; an
// error answer from the server never moves it; the next launch tries the server first again.
//   node mirror-proxy.mjs &   ACCOUNT=<number> node mirror.mjs
//   (test APK with OBSCURIFY_API=http://127.0.0.1:8094 DEVTOOLS=1; adb reverse tcp:8094 tcp:8094 and tcp:8095 tcp:8095)
import { launch, ok, sleep, adb, PKG } from './lib.mjs';

const ctl = (p) => fetch('http://127.0.0.1:8095' + p).then((r) => r.text());
const files = () => adb('shell', 'ls', `/data/user/0/${PKG}/files`).split(/\s+/).filter(Boolean);
const say = async (page, text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 15000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 240000 });
  await sleep(500);
  return page.locator('.msg').last().textContent();
};

await ctl('/__up');
adb('reverse', 'tcp:8094', 'tcp:8094');
let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 60000 });
for (let i = 0; i < 40 && !files().includes('mirrors.json'); i++) await sleep(500);
ok(files().includes('mirrors.json'), 'signed in, the app keeps the mirror list');
ok(/127\.0\.0\.1:8095/.test(adb('shell', 'cat', `/data/user/0/${PKG}/files/mirrors.json`)), 'it is the list the server gave');

ok(/pelican/i.test(await say(page, 'Reply with the single word PELICAN.')), 'server up: a chat is answered');
ok(!JSON.parse(await ctl('/__seen')).some((p) => /chat\/completions/.test(p)), 'server up: nothing went to the mirror');

// Unreachable as a block looks to the phone: nothing answers on the server's port (with adb reverse
// in place the phone's connect always succeeds, so the mapping itself goes).
adb('reverse', '--remove', 'tcp:8094');
// Connections already open stay up; the app lets idle ones go after 20 s.
await sleep(25000);
ok(/otter/i.test(await say(page, 'Reply with the single word OTTER.')), 'server unreachable: the chat is answered anyway');
const seen = JSON.parse(await ctl('/__seen'));
ok(seen.some((p) => /chat\/completions/.test(p)), `it went through the mirror (${seen.join(', ')})`);
ok(/heron/i.test(await say(page, 'Reply with the single word HERON.')), 'and stays there for this run');

// Next launch: the server first again.
adb('reverse', 'tcp:8094', 'tcp:8094');
const before = JSON.parse(await ctl('/__seen')).length;
({ page } = await launch(false));
await page.waitForSelector('#composer', { timeout: 60000 });
ok(/badger/i.test(await say(page, 'Reply with the single word BADGER.')), 'relaunched: a chat is answered');
const after = JSON.parse(await ctl('/__seen')).slice(before);
ok(!after.some((p) => /chat\/completions/.test(p)), 'relaunched with the server up: back on the server');
console.log('mirror: all passed');
process.exit(0);
