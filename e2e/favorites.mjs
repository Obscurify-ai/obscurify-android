// Favorites keep the order they were starred in and move up with ↑ (as on the web), and a new chat
// never shows a Web search chip, even in the first moments before the app has Code Mode's settings:
// web search follows the Tools switch, as on the web.
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh x86_64 && adb install -r <apk>
//   node e2e/flaky-proxy.mjs &  adb reverse tcp:8093 tcp:8093
//   ACCOUNT=<funded number> node e2e/favorites.mjs
import { launch, ok, sleep } from './lib.mjs';

const PROXY = 'http://127.0.0.1:8093';
const set = (q) => fetch(`${PROXY}/__set?${q}`);
await set('');

let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });
await sleep(1500);
const notNow = page.getByRole('button', { name: 'Not now' });
if (await notNow.count()) await notNow.click();

// 1. No Web search chip on a new chat, however late /code_mode answers.
const chipSeen = async (ms) => {
  for (let t = 0; t < ms; t += 150) {
    if (await page.locator('.composer .chip', { hasText: 'Web search' }).count().catch(() => 0)) return true;
    await sleep(150);
  }
  return false;
};
await set('codemodedelay=6000');
({ page } = await launch(false));
ok(!(await chipSeen(8000)), 'no Web search chip while Code Mode settings load');
await page.locator('.bar .icon', { hasText: '＋' }).click();
ok(!(await chipSeen(1500)), 'no Web search chip on a new chat');
await set('');

// 2. Favorites order.
await page.locator('.title.model').click();
await page.locator('.toggles:not(.providers) .chip', { hasText: /^All$/ }).click();
const names = () => page.locator('.drawer .pick .grow > div:first-child').allTextContents();
const [a, b, c] = (await names()).slice(0, 3);
// Starred oldest-first, so the starred order isn't the list's newest-first order.
for (const n of [c, b, a]) await page.locator('.drawer .pick', { hasText: n }).first().locator('.star').click();
await page.locator('.toggles:not(.providers) .chip', { hasText: /^Favorites$/ }).click();
let rows = await names();
ok(rows.join('|') === [c, b, a].join('|'), 'Favorites list in the order they were starred: ' + rows.join(', '));
const ups = await page.locator('.drawer .pick').evaluateAll((rs) => rs.map((r) => Boolean(r.querySelector('.up'))));
ok(ups.join() === 'false,true,true', 'every favorite but the first has ↑: ' + ups.join());
await page.locator('.drawer .pick', { hasText: a }).first().locator('.up').click();
rows = await names();
ok(rows.join('|') === [c, a, b].join('|'), '↑ moves it up one place: ' + rows.join(', '));
ok(await page.locator('.drawer .title', { hasText: 'Model' }).count() === 1, 'the picker stays open');
await page.locator('.drawer .pick', { hasText: a }).first().locator('.up').click();
await page.locator('input.search').fill(a.slice(0, 4));
ok(await page.locator('.drawer .up').count() === 0, 'no ↑ in search results');

// Kept after a restart, and the picker opens on Favorites.
({ page } = await launch(false));
await page.locator('.title.model').click();
await sleep(500);
rows = await names();
ok(rows.join('|') === [a, c, b].join('|'), 'order kept after a restart: ' + rows.join(', '));
ok(await page.locator('.toggles:not(.providers) .chip.on', { hasText: /^Favorites$/ }).count() === 1, 'the picker opens on Favorites');
await page.locator('.toggles:not(.providers) .chip', { hasText: /^All$/ }).click();
ok(await page.locator('.drawer .up').count() === 0, 'no ↑ outside Favorites');
await page.locator('.bar .icon', { hasText: '←' }).click();
console.log('PASS favorites');
process.exit(0);
