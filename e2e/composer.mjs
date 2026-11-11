// The message box grows with dictated or typed text (up to half the screen, then scrolls),
// and + and Send stay centred on it at any height. Sends nothing.
//   node e2e/composer.mjs      (test APK built with DEVTOOLS=1)
import { launch, ok, sleep, shot } from './lib.mjs';

const { page } = await launch(true);
await page.getByRole('button', { name: 'Chat as a guest' }).click();
await page.waitForSelector('#composer');
const boxes = () => Promise.all(['.composer .line', '#composer', '.plus', '.send'].map((s) => page.locator(s).boundingBox()));
const mid = (b) => b.y + b.height / 2;
const centred = ([line, , plus, send]) => Math.abs(mid(plus) - mid(line)) <= 1.5 && Math.abs(mid(send) - mid(line)) <= 1.5;

const [line0, box0, plus0, send0] = await boxes();
ok(centred([line0, box0, plus0, send0]), 'one line: + and Send centred: ' + JSON.stringify({ line0, plus0, send0 }));
const right = line0.x + line0.width - (send0.x + send0.width), top = send0.y - line0.y, bottom = line0.y + line0.height - (send0.y + send0.height);
ok(Math.abs(right - top) <= 2 && Math.abs(top - bottom) <= 1.5, `Send sits evenly in the round end: right ${right}, top ${top}, bottom ${bottom}`);

const words = 'issue with the text being too long and the box not showing what I said ';
await page.locator('#composer').click();
await page.locator('#composer').pressSequentially(words.repeat(2), { delay: 5 });
await sleep(300);
const [line1, box1, plus1, send1] = await boxes();
const fits = await page.evaluate(() => { const t = document.getElementById('composer'); return t.scrollHeight <= t.clientHeight + 1; });
ok(box1.height > box0.height * 1.8 && fits, `a few lines: the box grows to show them all (${box0.height} -> ${box1.height})`);
ok(centred([line1, box1, plus1, send1]), 'a few lines: + and Send centred');
shot('composer-lines');

await page.locator('#composer').fill(words.repeat(40));
await sleep(300);
const [line2, box2, plus2, send2] = await boxes();
const vh = await page.evaluate(() => innerHeight);
const scrolls = await page.evaluate(() => { const t = document.getElementById('composer'); return t.scrollHeight > t.clientHeight; });
ok(box2.height <= vh / 2 + 1 && box2.height > box1.height && scrolls, `a long message: the box stops at half the screen (${box2.height} of ${vh}) and scrolls`);
ok(centred([line2, box2, plus2, send2]), 'a long message: + and Send centred');
shot('composer-long');

await page.locator('#composer').fill('');
await sleep(300);
const [, box3] = await boxes();
ok(Math.abs(box3.height - box0.height) <= 1, `emptied, the box is one line again (${box3.height})`);
process.exit(0);
