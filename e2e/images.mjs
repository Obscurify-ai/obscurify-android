// Images only for models that take them (the model list's `images`): picking a photo on a
// text-only model attaches nothing and says "This model does not support images."; switching to
// one with a photo attached and pressing Send says so too, sends nothing, and keeps the text.
// Against the live server with a prod build (DEVTOOLS=1 ./build.sh x86_64). CODE is a funded
// account code; TEXT_ONLY / SEES pick the models (defaults below).
//   CODE=XXXXX-... node e2e/images.mjs
import { writeFileSync } from 'node:fs';
import { launch, ok, shot, sleep } from './lib.mjs';

const TEXT_ONLY = process.env.TEXT_ONLY || 'Clover 2';
const SEES = process.env.SEES || 'Gemini 2.5 Flash Lite';
const png = '/tmp/obs-images-e2e.png';
writeFileSync(png, Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP8z8DAwMDAxMDAwMDAAAANHQEDasKb6QAAAABJRU5ErkJggg==', 'base64'));

const { page } = await launch(true);
await page.locator('.login input').fill(process.env.CODE);
await page.locator('.login button.go').click();
await page.waitForSelector('#composer', { timeout: 90000 });
if (await page.locator('#code-upgrade-later, #enter-code-later').count()) await page.locator('#code-upgrade-later, #enter-code-later').first().click().catch(() => {});

const pickModel = async (name) => {
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(name);
  await page.locator('.pick', { hasText: name }).first().locator('.grow').click();
  await sleep(500);
};
const attach = async () => {
  await page.locator('#pick').setInputFiles(png);
  await sleep(1500);
};
const thumbs = () => page.locator('.thumbs img').count();
const toast = async () => (await page.locator('.toast').allInnerTexts().catch(() => [])).join(' ');

await pickModel(TEXT_ONLY);
await attach();
ok((await thumbs()) === 0, `${TEXT_ONLY}: the photo is not attached`);
ok((await toast()).includes('This model does not support images.'), '  and the notice says why: ' + (await toast()));
shot('images-not-supported');

await sleep(4000); // the notice goes
await pickModel(SEES);
await attach();
ok((await thumbs()) === 1, `${SEES}: the photo is attached`);

await pickModel(TEXT_ONLY);
await page.locator('#composer').fill('what is this?');
await sleep(300);
await page.locator('.send').click();
await sleep(800);
ok((await toast()).includes('This model does not support images.'), 'Send on a text-only model with a photo attached: the notice');
ok((await page.locator('.msg.user').count()) === 0, '  nothing was sent');
ok((await page.locator('#composer').inputValue()).includes('what is this?'), '  the text stays in the box');
console.log('\nall image checks passed');
process.exit(0);
