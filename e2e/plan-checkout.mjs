// One-step plan checkout: Upgrade with too little on the balance goes straight to paying for the plan
// (Monero first, for the plan's price, no Continue or Add funds button), each choice of method or term
// starts that payment at once, and the plan switches on by itself once it's paid. Needs a dev server
// whose new accounts start with nothing (FREE_BALANCE=0), the regtest BTCPay stack and Stripe test keys:
//   OBSCURIFY_API=http://127.0.0.1:8241 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8241 tcp:8241 && node e2e/plan-checkout.mjs
import { execFileSync } from 'node:child_process';
import { launch, ok, shot, sleep, wordsOn, device } from './lib.mjs';

const btc = (...a) => execFileSync('docker', ['exec', process.env.BTC_CONTAINER || 'obscurify-bitcoind-dev', 'bitcoin-cli', '-regtest', '-rpcuser=btcrpc',
  `-rpcpassword=${process.env.BTC_RPC_PASSWORD || 'devpassword'}`, '-rpcwallet=payer', ...a], { encoding: 'utf8' }).trim();
const { page } = await launch(true);

await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
await wordsOn(page, '.number', 20000);
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');

const toAccount = async () => {
  await page.locator('.bar .icon').first().click();
  await page.locator('.foot button', { hasText: 'Account' }).click();
  await page.waitForSelector('.big');
};
const casual = page.locator('.section .row', { hasText: /Casual · \$20\.00\/mo/ });
await toAccount();
await casual.waitFor({ timeout: 20000 });
await casual.getByRole('button', { name: 'Upgrade' }).click();

// Straight to the payment: Monero, for the plan's price. The dev store has no Monero wallet, so
// either the address or the server's "unavailable" shows that the payment was asked for.
await page.locator('.title', { hasText: 'Casual' }).waitFor({ timeout: 15000 });
ok(true, 'Upgrade goes straight to paying for Casual');
ok(await page.locator('.seg button.on').textContent() === 'Monero', 'Monero first');
await page.waitForFunction(() => document.querySelector('.qr svg') || /XMR payments are unavailable/.test(document.body.textContent), null, { timeout: 30000 });
ok(true, 'the Monero payment started by itself');
ok(await page.getByText('Continue', { exact: true }).count() === 0 && await page.getByText('Add funds', { exact: true }).count() === 0, 'no Continue or Add funds button');
ok(await page.locator('.plan-term .chip.on').textContent() === '1 month · $20.00', 'one month, $20.00');
ok(await page.locator('.blind-toggle').count() === 0, 'no blind voucher for a plan');
shot('plan-checkout-xmr');

const heading = re => page.waitForFunction(r => new RegExp(r).test((document.querySelector('.lead') || {}).textContent || ''), re, { timeout: 30000 });
await page.getByRole('button', { name: 'Bitcoin' }).click();
await heading('Send [0-9.]+ BTC \\(about \\$20\\.00\\)');
const month = await page.locator('.lead').textContent();
ok(true, 'Bitcoin, one tap: ' + month);
ok(/Casual switches on when the payment confirms/.test(await page.locator('.scroll').textContent()), 'says Casual switches on');
shot('plan-checkout-btc');

await page.locator('.plan-term .chip', { hasText: '12 months' }).click();
await heading('about \\$200\\.00');
ok(true, '12 months: ' + await page.locator('.lead').textContent());

await page.getByRole('button', { name: 'Card' }).click();
const pay = page.getByRole('button', { name: /^Pay \$200\.00$/ });
await pay.waitFor({ timeout: 30000 });
await page.locator('#stripe iframe').first().waitFor({ timeout: 30000 });
ok(await page.locator('.qr').count() === 0, 'Card: the card form for $200.00, no address');
await page.locator('.plan-term .chip', { hasText: '1 month' }).click();
await page.getByRole('button', { name: /^Pay \$20\.00$/ }).waitFor({ timeout: 30000 });
await page.locator('#stripe iframe').first().waitFor({ timeout: 30000 });
await sleep(2000);
ok(await page.locator('#stripe > div').count() === 1, 'one card form for the new term');
// Stripe is loaded now, so the form can be asked for before its place is drawn: every time, it shows.
for (let i = 0; i < 10; i++) {
  const year = i % 2 === 0;
  await page.locator('.plan-term .chip', { hasText: year ? '12 months' : '1 month' }).click();
  await page.locator('button:enabled', { hasText: year ? /^Pay \$200\.00$/ : /^Pay \$20\.00$/ }).waitFor({ timeout: 15000 });
  await page.locator('#stripe iframe').first().waitFor({ timeout: 15000 });
}
ok(await page.locator('.err').count() === 0, 'the card form loads on each of 10 changes of term');
shot('plan-checkout-card');

// Quick taps: only the last choice is shown.
await page.getByRole('button', { name: 'Lightning' }).click();
await page.getByRole('button', { name: 'Card' }).click();
await page.getByRole('button', { name: 'Bitcoin' }).click();
await heading('about \\$20\\.00');
await sleep(6000);
ok(await page.locator('#stripe').count() === 0 && await page.locator('.qr svg').count() === 1, 'quick taps: only the last choice is shown');
ok(await page.locator('.lead').textContent() === month, 'the Bitcoin payment opened before is shown again');

// Pay it.
const addr = (await page.locator('p.mono').first().textContent()).trim();
const amount = month.match(/Send ([0-9.]+) BTC/)[1];
btc('-named', 'sendtoaddress', `address=${addr}`, `amount=${amount}`, 'fee_rate=10');
btc('generatetoaddress', '1', btc('getnewaddress'));
const t0 = Date.now();
await page.locator('.title', { hasText: 'Account' }).waitFor({ timeout: 300000 });
ok(true, `paid: back on Account after ${Math.round((Date.now() - t0) / 1000)} s`);
await page.locator('.section .row', { hasText: /Casual · \$20\.00\/mo/ }).locator('.ok', { hasText: 'Current' }).waitFor({ timeout: 20000 });
ok(true, 'Casual is the current plan');
shot('plan-checkout-on');

// Upgrading again: Pro, less this month's Casual.
await page.locator('.section .row', { hasText: /Pro · \$45\.00\/mo/ }).getByRole('button', { name: 'Upgrade' }).click();
await page.locator('.title', { hasText: 'Pro' }).waitFor({ timeout: 15000 });
await page.getByRole('button', { name: 'Bitcoin' }).click();
await heading('about \\$25\\.00');
ok(true, 'Pro from Casual asks for the difference: ' + await page.locator('.lead').textContent());

// Add funds is as it was, with the new minimums.
await page.locator('.bar .icon').first().click();
await page.getByText('Add funds', { exact: true }).click();
await page.waitForSelector('.blind-toggle');
await page.getByRole('button', { name: 'Card' }).click();
await page.locator('input.field').fill('4');
await page.getByText('Continue', { exact: true }).click();
ok(await page.getByText('from $5 to $10,000', { exact: false }).count() === 1, 'a $4 card payment is refused');
await page.locator('input.field').fill('5');
await page.getByText('Continue', { exact: true }).click();
await page.getByRole('button', { name: /^Pay \$5\.00$/ }).waitFor({ timeout: 30000 });
ok(true, 'a $5 card payment opens');
console.log('all passed');
await device.close();
process.exit(0);
