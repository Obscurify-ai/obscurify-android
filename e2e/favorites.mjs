// Favorites keep the order they were starred in and are dragged into order by their handles, in the
// picker (as on the web), a new chat starts on the first favorite, and a new chat never shows a Web search chip, even in the first moments before the app has Code Mode's settings:
// web search follows the Tools switch, as on the web.
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh x86_64 && adb install -r <apk>
//   node e2e/flaky-proxy.mjs &  adb reverse tcp:8093 tcp:8093
//   ACCOUNT=<funded number> node e2e/favorites.mjs
import { adb, launch, ok, PKG, shot, sleep } from './lib.mjs';

// SKIP_FLAKY=1: against a server without flaky-proxy; the slow /code_mode check is skipped.
const PROXY = 'http://127.0.0.1:8093';
const set = (q) => process.env.SKIP_FLAKY ? null : fetch(`${PROXY}/__set?${q}`);
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
const names = () => page.locator('.drawer .pick .grow > div:first-child').evaluateAll((ds) => ds.map((d) => d.firstChild.textContent.trim()));
const [a, b, c, d] = (await names()).slice(0, 4);
const row = (n) => page.locator('.drawer .pick').filter({ has: page.locator('.grow > div:first-child').filter({ hasText: n }) }).first();
// Starred oldest-first, so the starred order isn't the list's newest-first order.
for (const n of [d, c, b, a]) await row(n).locator('.star').click();
ok(await page.locator('.drawer .handle').count() === 0, 'no handles outside Favorites');
await page.locator('.toggles:not(.providers) .chip', { hasText: /^Favorites$/ }).click();
let rows = await names();
ok(rows.join('|') === [d, c, b, a].join('|'), 'Favorites list in the order they were starred: ' + rows.join(', '));
ok(await page.locator('.drawer .handle').count() === 4, 'every favorite has a handle');
const badged = () => page.locator('.drawer .pick').evaluateAll((rs) => rs.findIndex((r) => r.querySelector('.new-chats')));
ok(await badged() === 0, 'the first is marked for new chats');
shot('favorites-1');

// A real finger (adb) on the handle: down, moves, up.
const dpr = await page.evaluate(() => devicePixelRatio);
// The page starts under the status bar.
const bar = +(adb('shell', 'dumpsys', 'window').match(/type=statusBars frame=\[0,0\]\[\d+,(\d+)\]/) || [0, 0])[1];
async function drag(from, to, mid) {
  const h = await page.locator('.drawer .handle').nth(from).boundingBox();
  const t = await page.locator('.drawer .pick').nth(to).boundingBox();
  const x = Math.round((h.x + h.width / 2) * dpr), y0 = h.y + h.height / 2, y1 = to < from ? t.y + 6 : t.y + t.height - 6;
  const at = (y) => String(Math.round(y * dpr) + bar);
  const ev = (kind, y) => adb('shell', 'input', 'touchscreen', 'motionevent', kind, String(x), at(y));
  ev('DOWN', y0);
  for (let i = 1; i <= 6; i++) { ev('MOVE', y0 + (y1 - y0) * i / 6); if (mid && i === 3) { await sleep(250); shot(mid); } }
  ev('UP', y1);
  await sleep(500);
}
await drag(3, 0, 'favorites-2');
rows = await names();
ok(rows.join('|') === [a, d, c, b].join('|'), 'dragged from last to first: ' + rows.join(', '));
ok(await badged() === 0, 'the new first is marked for new chats');
ok(await page.locator('.drawer .pick').evaluateAll((rs) => rs.every((r) => !r.style.transform && !r.dataset.dragging)), 'nothing left shifted');
ok(await page.locator('.drawer .title', { hasText: 'Model' }).count() === 1, 'the picker stays open');
shot('favorites-3');
await drag(0, 2);
rows = await names();
ok(rows.join('|') === [d, c, a, b].join('|'), 'dragged from first to third: ' + rows.join(', '));
await page.locator('.drawer .handle').nth(2).click();
ok(await page.locator('.drawer .title', { hasText: 'Model' }).count() === 1, 'a tap on the handle picks nothing');
await page.locator('.drawer .handle').nth(2).focus();
await page.keyboard.press('ArrowUp');
await sleep(300);
rows = await names();
ok(rows.join('|') === [d, a, c, b].join('|'), 'the up key on the handle moves it up: ' + rows.join(', '));
await drag(1, 0);
await page.locator('input.search').fill(a.slice(0, 4));
ok(await page.locator('.drawer .handle').count() === 0, 'no handles in search results');

// Kept after a restart, and the picker opens on Favorites.
({ page } = await launch(false));
await page.locator('.title.model').click();
await sleep(500);
rows = await names();
ok(rows.join('|') === [a, d, c, b].join('|'), 'order kept after a restart: ' + rows.join(', '));
ok(await page.locator('.toggles:not(.providers) .chip.on', { hasText: /^Favorites$/ }).count() === 1, 'the picker opens on Favorites');
await page.locator('.bar .icon', { hasText: '←' }).click();

// Needs adb root (reads and seeds the app's settings).
const SETTINGS = `/data/data/${PKG}/files/settings.json`;
const saved = () => JSON.parse(adb('shell', 'cat', SETTINGS)).favorites;
const ids = saved();

// A favorite no longer offered stays in its place, says so, can't be picked, can be moved and unstarred.
const seed = async (favorites) => {
  adb('shell', 'am', 'force-stop', PKG);
  const v = JSON.stringify({ ...JSON.parse(adb('shell', 'cat', SETTINGS)), favorites });
  adb('shell', `echo ${Buffer.from(v).toString('base64')} | base64 -d > ${SETTINGS}`);
  ({ page } = await launch(false));
  await page.locator('.title.model').click();
  await page.locator('.drawer .pick').first().waitFor();
  await sleep(1500);
};
await seed(['gone/model', ...ids]);
rows = await names();
ok(rows[0] === 'gone/model' && rows.length === 5, 'a favorite no longer offered is listed in its place: ' + rows.join(', '));
ok((await page.locator('.drawer .pick').first().innerText()).includes('No longer offered'), 'and says so');
ok(await badged() === 1, 'new chats skip it');
await page.locator('.drawer .pick .grow').first().click();
await sleep(300);
ok(await page.locator('.drawer .title', { hasText: 'Model' }).count() === 1, 'and it is not picked');
await drag(0, 2);
ok(saved().indexOf('gone/model') === 2, 'it can be dragged: ' + saved().join(', '));
// The row is aria-disabled (it can't be picked), which Playwright takes to cover its star.
await page.locator('.drawer .pick').nth(2).locator('.star').click({ force: true });
await sleep(400);
ok(saved().join() === ids.join(), 'and unstarred');
const prov = (await names())[0].split(':')[0];
await page.locator('.toggles.providers .chip', { hasText: prov }).first().click();
await sleep(300);
ok((await names()).every((n) => n.startsWith(prov)), 'a provider narrows the favorites: ' + (await names()).join(', '));

// Twenty favorites: held at the bottom edge, the list scrolls and the row lands last.
const all = JSON.parse(await page.evaluate(async (u) => (await (await fetch(u)).text()), process.env.API_MODELS || 'http://127.0.0.1:8242/v1/models').catch(async () => await (await fetch(process.env.API_MODELS || 'http://127.0.0.1:8242/v1/models')).text())).data.filter((m) => !m.decisions);
const many = all.slice(0, 20).map((m) => m.id);
await seed(many);
ok((await names()).length === 20, 'twenty listed');
{
  const list = await page.locator('.drawer .scroll').boundingBox();
  const h = await page.locator('.drawer .handle').first().boundingBox();
  const x = String(Math.round((h.x + h.width / 2) * dpr)), at = (y) => String(Math.round(y * dpr) + bar);
  const ev = (kind, y) => adb('shell', 'input', 'touchscreen', 'motionevent', kind, x, at(y));
  ev('DOWN', h.y + h.height / 2);
  ev('MOVE', h.y + 120);
  ev('MOVE', list.y + list.height - 25);
  await sleep(6000);
  shot('favorites-scroll');
  ev('UP', list.y + list.height - 25);
  await sleep(600);
  const after = saved();
  ok(after.join() === [...many.slice(1), many[0]].join(), 'a long list scrolls while dragging: lands at ' + (after.indexOf(many[0]) + 1) + ' of 20');
}
await seed(ids);
await page.locator('.bar .icon', { hasText: '←' }).click();

// 3. New chats start on the first favorite (a), even after another model was picked.
const short = (n) => n.replace(/^[^:]*:\s*/, '');
const title = async () => (await page.locator('.title.model').innerText()).replace(' ▾', '').trim();
await page.locator('.title.model').click();
await page.locator('.toggles:not(.providers) .chip', { hasText: /^Favorites$/ }).click();
await page.locator('.drawer .pick', { hasText: b }).first().locator('.grow').click();
await sleep(400);
ok(short(b).includes(short(await title())) || (await title()).includes(short(b)), 'picked another favorite: ' + await title());
await page.locator('.bar .icon', { hasText: '＋' }).click();
await sleep(400);
ok(short(a).includes(short(await title())) || (await title()).includes(short(a)), 'a new chat starts on the first favorite: ' + await title());
console.log('PASS favorites');
process.exit(0);
