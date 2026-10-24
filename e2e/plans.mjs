// End-to-end test of the model picker's plan ordering: models the plan can use first, the rest under
// "Upgrade to use"; categories with nothing usable after the others. Needs an existing account on
// the dev server (build and install as in app.mjs):
//   ACCOUNT=<16 digits> PLAN=pay_as_you_go|casual|pro node e2e/plans.mjs
// PLAN is the account's plan (set it in the dev database first).
import { launch, ok, shot } from './lib.mjs';

const plan = process.env.PLAN || 'pay_as_you_go';
const { page } = await launch(true);

await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer');
await page.waitForTimeout(3000); // the catalogue and the plan load in the background

await page.locator('.title.model').click();
await page.locator('.chip', { hasText: 'All' }).first().click();
const chips = await page.locator('.drawer .toggles .chip').evaluateAll(cs => cs.map(c => ({ k: c.textContent, locked: c.classList.contains('locked') })));
const firstLocked = chips.findIndex(c => c.locked);
ok(firstLocked === -1 || chips.slice(firstLocked).every(c => c.locked), 'locked categories come last: ' + chips.map(c => c.k + (c.locked ? '🔒' : '')).join(' '));
const lockedChips = chips.filter(c => c.locked).map(c => c.k).sort().join(',');
const expected = { pay_as_you_go: 'Frontier,Uncensored', casual: 'Uncensored', pro: '' }[plan];
ok(lockedChips === expected, `${plan}: locked categories are [${expected}]`);

const rows = await page.locator('.scroll > *').evaluateAll(es => es.map(e => e.tagName === 'H2' ? '##' : e.classList.contains('locked') ? 'X' : 'ok'));
const head = rows.indexOf('##');
ok(head === -1 ? plan === 'pro' : rows.slice(0, head).every(r => r === 'ok') && rows.slice(head + 1).every(r => r === 'X'),
  `usable models first (${head === -1 ? rows.length : head}), then ${head === -1 ? 0 : rows.length - head - 1} under "Upgrade to use"`);
shot(`plans-${plan}-all`);

if (head !== -1) {
  // A locked model isn't chosen: it says which plan it needs and opens Account.
  const before = await page.locator('.pick.on').count();
  await page.locator('.pick.locked .grow').first().click();
  await page.waitForSelector('text=Plan', { timeout: 10000 });
  ok(await page.locator('.title', { hasText: 'Account' }).count() === 1, 'a locked model opens Account');
  shot(`plans-${plan}-account`);
  await page.locator('.bar .icon').first().click();
  await page.locator('.title.model').click();
  const uncensored = page.locator('.chip', { hasText: 'Uncensored' });
  if (await uncensored.count()) {
    await uncensored.click();
    shot(`plans-${plan}-uncensored`);
  }
  ok(await page.locator('.pick.on').count() <= Math.max(before, 1), 'the chosen model did not change');
}
console.log('all ok');
process.exit(0);
