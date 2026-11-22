// A model the list says can't answer right now (`available: false`) is greyed out in the picker and
// can't be chosen; it greys out, and comes back, when the picker opens again, and a chat already on
// it keeps it. Runs its own proxy on 8097 that marks MODEL down (test APK with
// OBSCURIFY_API=http://127.0.0.1:8097, `adb reverse tcp:8097 tcp:8097`). CODE is an account code there.
//   TARGET=http://127.0.0.1:8098 CODE=XXXXX-... node e2e/unavailable.mjs
import http from 'node:http';
import { launch, ok, shot, sleep } from './lib.mjs';

const TARGET = new URL(process.env.TARGET || 'http://127.0.0.1:8098');
const MODEL = process.env.MODEL || 'google/gemini-2.5-flash-lite';
const NAME = process.env.NAME || 'Gemini 2.5 Flash Lite';
const OTHER = process.env.OTHER || 'Clover 2';
const PORT = Number(process.env.PORT || 8097); // the host end of the adb reverse
let down = true;
http.createServer((req, res) => {
  const up = http.request({ host: TARGET.hostname, port: TARGET.port, method: req.method, path: req.url, headers: { ...req.headers, host: TARGET.host, 'accept-encoding': 'identity' } }, r => {
    if (!/^(\/v1)?\/models(\?|$)/.test(req.url)) { res.writeHead(r.statusCode, r.headers); return r.pipe(res); }
    let body = '';
    r.setEncoding('utf8').on('data', d => body += d).on('end', () => {
      const list = JSON.parse(body);
      for (const m of list.data) if (m.id === MODEL) m.available = !down;
      res.writeHead(r.statusCode, { 'content-type': 'application/json' }).end(JSON.stringify(list));
    });
  });
  up.on('error', () => res.writeHead(502).end());
  req.pipe(up);
}).listen(PORT, '127.0.0.1');

const { page } = await launch(true);
await page.locator('.login input').fill(process.env.CODE);
await page.locator('.login button.go').click();
await page.waitForSelector('#composer', { timeout: 90000 });
for (const sel of ['#code-upgrade-later', '#enter-code-later']) if (await page.locator(sel).count()) await page.locator(sel).first().click().catch(() => {});

const open = async name => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(name);
  await sleep(2500);
};
const back = async () => { await page.locator('.drawer .bar .icon').first().click(); await sleep(400); };
const row = name => page.locator('.pick', { hasText: name }).first();
const title = async () => (await page.locator('.title.model').innerText()).trim();

await open(NAME);
ok(await row(NAME).count() === 1, 'a model that is down is still listed');
ok((await row(NAME).getAttribute('class')).includes('locked'), '  greyed out');
ok((await row(NAME).innerText()).includes('(Unavailable)'), '  and says Unavailable');
shot('unavailable');
await row(NAME).locator('.grow').click();
await sleep(500);
const toast = (await page.locator('.toast').allInnerTexts().catch(() => [])).join(' ');
ok(toast.includes('is unavailable right now'), '  choosing it says so: ' + toast);
ok(await page.locator('input.search').count() === 1, '  and leaves the picker open');
await back();
ok(!(await title()).includes(NAME), '  with the model unchanged: ' + await title());

await open(OTHER);
await row(OTHER).locator('.grow').click();
await sleep(500);
ok((await title()).includes(OTHER), 'another model can be chosen');

down = false;
await open(NAME);
ok(!(await row(NAME).innerText()).includes('(Unavailable)'), 'it comes back when the picker opens again');
await row(NAME).locator('.grow').click();
await sleep(500);
ok((await title()).includes(NAME), '  and can be chosen');

down = true;
await open(NAME);
ok((await row(NAME).innerText()).includes('(Unavailable)'), 'it greys out when the picker opens again');
await back();
ok((await title()).includes(NAME), '  and a chat already on it keeps it');
console.log('\nall unavailable-model checks passed');
process.exit(0);
