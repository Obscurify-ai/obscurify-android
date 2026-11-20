// The Google Play build fails closed: no way to pay, no uncensored or roleplay models, nothing that
// sells plans, the content guard on every completion, and Report on answers. Everything goes through
// flaky-proxy.mjs, which records each request the app makes.
//   OBSCURIFY_API=http://127.0.0.1:8093 DEVTOOLS=1 ./build.sh play x86_64 && adb install -r target/obscurify-play.apk
//   node e2e/flaky-proxy.mjs &  adb reverse tcp:8093 tcp:8093
//   PKG=ai.obscurify.play ACCOUNT=<funded number> node e2e/play.mjs
// Sends two short chats (a cent at most).
import { launch, agentSwitches, ok, sleep } from './lib.mjs';

const PROXY = 'http://127.0.0.1:8093';
const proxy = (p) => fetch(PROXY + p).then((r) => r.json().catch(() => null));
const GUARD = 'Never write sexually explicit content';
// What the Play build must never show or offer.
const LEFT_OUT = /uncensored|abliterated|venice|thedrummer|sao10k|anthracite|magnum|euryale|cydonia|rocinante|mythomax|lumimaid|nsfw|lewd|erotic|heretic|roleplay/i;
const PAYING = /top_ups|vouchers|\/orders|create_top_up|available_tiers|referral_info|stripe|checkout|change_tier|subscribe/i;
await fetch(PROXY + '/__set');

let { page } = await launch(true);

// Sign-up: no referral code (the referral program pays out).
await page.getByText('Create one').click();
ok(await page.locator('input[placeholder^="Referral"]').count() === 0, 'sign-up: no referral code');
ok(await page.getByText("I'm 18 or older").count() === 1, 'sign-up: 18+');
await page.locator('a', { hasText: /^Sign in$/ }).click();

await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });
await sleep(2000);
const notNow = page.getByRole('button', { name: 'Not now' });
if (await notNow.count()) await notNow.click();

// Models: none left out, even by search, and no Uncensored or Roleplay category.
await page.locator('.title.model').click();
await page.locator('.toggles:not(.providers) .chip', { hasText: /^All$/ }).click();
const chips = await page.locator('.toggles:not(.providers) .chip').allTextContents();
ok(!chips.some((c) => /Uncensored|Roleplay/.test(c)), 'no Uncensored or Roleplay category: ' + chips.join(', '));
const names = await page.locator('.drawer .pick .grow > div:first-child').allTextContents();
ok(names.length > 20, `the picker lists models (${names.length})`);
ok(!names.some((n) => LEFT_OUT.test(n)), 'no uncensored or roleplay model listed: ' + names.filter((n) => LEFT_OUT.test(n)).join(', '));
for (const q of ['uncensored', 'venice', 'abliterated', 'drummer', 'magnum', 'euryale']) {
  await page.locator('input.search').fill(q);
  await sleep(200);
  ok(await page.locator('.drawer .pick').count() === 0, `search "${q}" finds nothing`);
}
await page.locator('.bar .icon', { hasText: '←' }).click();

// Drawer: no Tor, no API or website pages that sell plans; the legal links stay.
await page.locator('.bar .icon', { hasText: '☰' }).click();
ok(await page.locator('#tor').count() === 0, 'drawer: no Tor');
ok(await page.locator('.foot a[href$="/api-docs"], .foot .social, .foot details.more').count() === 0, 'drawer: no API, social or More info links');
const legal = await page.locator('.foot .legal a').allTextContents();
ok(legal.includes('Support') && legal.includes('Privacy') && legal.includes('Terms'), 'drawer: ' + legal.join(', '));

// Account: says where to pay, as plain text; no Add funds, plans to switch or referrals.
await page.locator('.foot .account').click();
await page.waitForSelector('h2:has-text("Balance")');
await sleep(2500);
const account = await page.locator('.scroll').first().innerText();
ok(await page.getByRole('button', { name: /Add funds/ }).count() === 0, 'account: no Add funds');
ok(account.includes('Add funds and change plans on the website, obscurify.ai.'), 'account: says where to pay, as text');
ok(await page.locator('.section a[href*="obscurify.ai"]').count() === 0, 'account: no link to pay');
ok(await page.getByRole('button', { name: /^(Pay|Switch|Upgrade)/ }).count() === 0 && !/Referrals/.test(account), 'account: no plan buttons, no referrals');
ok(/Delete account/.test(account), 'account: Delete account');
await page.locator('.bar .icon', { hasText: '←' }).click();

// Chats, with Tools on (Code Mode) and both switches off (plain chat): the guard goes first.
const ask = async (text) => {
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 15000 });
  await page.waitForSelector('.send:not(.stop)', { timeout: 150000 });
};
await ask('Reply with the single word: alpha');
await page.locator('.bar .icon', { hasText: '＋' }).click();
await agentSwitches(page, false);
await ask('Reply with the single word: beta');
const sent = await proxy('/__log');
ok(sent.length >= 2, `completions sent (${sent.length})`);
const fresh = sent.filter((b) => !b.thread || !b.thread.rev);
ok(fresh.length >= 2 && fresh.every((b) => b.messages[0]?.role === 'system' && b.messages[0].content.startsWith(GUARD)),
  'the guard is first in every new chat request: ' + fresh.map((b) => b.messages[0]?.content.slice(0, 30)).join(' | '));
ok(sent.every((b) => !LEFT_OUT.test(b.model)), 'no left-out model was asked: ' + sent.map((b) => b.model).join(', '));
await agentSwitches(page, true);

// Report on an answer.
await page.locator('.msg.assistant .report-btn').last().click();
ok(await page.locator('.report .chip').count() === 6, 'Report offers the reasons');
await page.locator('.report button', { hasText: 'Cancel' }).click();

// Nothing the app asked for was a payment, order, top-up, plan or referral endpoint.
const paths = await proxy('/__paths');
const paying = paths.filter((p) => PAYING.test(p));
ok(paying.length === 0, 'no payment, voucher, top-up, plan or referral requests: ' + paying.join(', '));
console.log('PASS play');
process.exit(0);
