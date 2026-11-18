// The app opens on the plan's default model, as the web chat does on each page load: Pro on GLM 5.3
// Uncensored, Casual on Claude Opus 5.5 (the server's DEFAULT_PRO_MODEL / DEFAULT_PAID_MODEL, sent
// as `default_models` by /account_tier). A model picked by hand lasts until the app restarts; a
// favorite beats the plan's default. Needs a Pro and a Casual account on the dev server (set
// account_tier in the dev database), and the app built against it:
//   OBSCURIFY_API=https://<dev server> DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   PRO=<16 digits> CASUAL=<16 digits> node e2e/plan-default.mjs
import { launch, ok, shot, sleep } from './lib.mjs';

const title = async (page) => (await page.locator('.title.model').innerText()).replace(' ▾', '').trim();
const settle = async (page, want) => {
  for (let i = 0; i < 40 && !(await title(page)).includes(want); i++) await sleep(500);
  return title(page);
};

async function signIn(account) {
  const { page } = await launch(true);
  await page.locator('.login input').fill(account);
  await page.getByRole('button', { name: 'Sign In' }).click();
  await page.waitForSelector('#composer', { timeout: 30000 });
  const notNow = page.getByRole('button', { name: 'Not now' });
  await sleep(1500);
  if (await notNow.count()) await notNow.click();
  return page;
}

async function pickOther(page, avoid) {
  await page.locator('.title.model').click();
  await page.locator('.toggles:not(.providers) .chip', { hasText: /^All$/ }).click();
  const row = page.locator('.drawer .pick:not(.locked):not(.on)').filter({ hasNotText: avoid }).first();
  const name = (await row.locator('.grow > div:first-child').innerText()).trim();
  await row.locator('.grow').click();
  return name;
}

// Pro: GLM 5.3 Uncensored, again after a restart even when another model was picked.
let page = await signIn(process.env.PRO);
let t = await settle(page, 'GLM 5.3 Uncensored');
ok(t.includes('GLM 5.3 Uncensored') && !t.includes('Flash'), `Pro opens on GLM 5.3 Uncensored: ${t}`);
shot('plan-default-pro');
const other = await pickOther(page, 'GLM 5.3 Uncensored');
await sleep(500);
ok(!(await title(page)).includes('GLM 5.3 Uncensored'), `a hand pick sticks for now: ${await title(page)}`);
({ page } = await launch(false));
t = await settle(page, 'GLM 5.3 Uncensored');
ok(t.includes('GLM 5.3 Uncensored'), `a restart opens on the plan's default again: ${t} (picked ${other})`);

// Casual: Claude Opus 5.5.
page = await signIn(process.env.CASUAL);
t = await settle(page, 'Opus 5.5');
ok(t.includes('Opus 5.5'), `Casual opens on Claude Opus 5.5: ${t}`);
shot('plan-default-casual');

// A favorite beats the plan's default.
await page.locator('.title.model').click();
await page.locator('.toggles:not(.providers) .chip', { hasText: /^All$/ }).click();
const fav = page.locator('.drawer .pick:not(.locked)').filter({ hasNotText: 'Opus 5.5' }).first();
const favName = (await fav.locator('.grow > div:first-child').innerText()).trim().replace(/^[^:]*:\s*/, '');
await fav.locator('.star').click();
({ page } = await launch(false));
t = await settle(page, favName);
ok(t.includes(favName), `with a favorite, the app opens on it: ${t}`);

console.log('all ok');
process.exit(0);
