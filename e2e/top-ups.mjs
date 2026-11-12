// Top-ups, the default way to add funds: the payment is tied to the account and the server credits
// it by itself, so the app can be closed while it confirms. Against a dev server with the regtest
// BTCPay stack; build with OBSCURIFY_API=… DEVTOOLS=1 ./build.sh x86_64, then
//   node e2e/top-ups.mjs   (BTC_RPC_PASSWORD, default devpassword; BTC_CONTAINER, default obscurify-bitcoind-dev)
// Steps: sign up, Add funds starts with Blind voucher off, a BTC top-up with no amount shows its
// address, the app is force-stopped, the regtest wallet pays and mines a block, the relaunched app
// lists it as Added and the balance is above 0. Then Blind voucher on: the warning, and Continue asks first.
import { execFileSync } from 'node:child_process';
import { launch, ok, shot, device, adb, PKG } from './lib.mjs';

const btc = (...a) => execFileSync('docker', ['exec', process.env.BTC_CONTAINER || 'obscurify-bitcoind-dev', 'bitcoin-cli', '-regtest', '-rpcuser=btcrpc',
  `-rpcpassword=${process.env.BTC_RPC_PASSWORD || 'devpassword'}`, '-rpcwallet=payer', ...a], { encoding: 'utf8' }).trim();
const toFunds = async page => {
  await page.locator('.bar .icon').first().click();
  await page.locator('.drawer .foot button').click();
  await page.getByText('Add funds').click();
  await page.waitForSelector('.blind-toggle');
};

let { page } = await launch(true);

// 1. A new account.
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Generate Account Number' }).click();
await page.locator('.number').waitFor({ timeout: 20000 });
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
ok(true, 'signed up');

// 2. Add funds: a top-up unless Blind voucher is turned on.
await toFunds(page);
ok(!(await page.locator('.blind-toggle input').isChecked()), 'Blind voucher is off by default');
ok(await page.locator('.warn').count() === 0, 'no blind warning while it is off');
ok(await page.getByText('even if you close the app', { exact: false }).count() === 1, 'copy describes a top-up');

// 3. A BTC top-up with no amount: the address and QR, and copy that says the app can close.
await page.getByRole('button', { name: 'Bitcoin' }).click();
await page.locator('input.field').fill('');
await page.getByText('Continue').click();
const addr = (await page.locator('p.mono').first().textContent({ timeout: 30000 })).trim();
ok(addr.startsWith('bcrt1'), 'BTC address shown: ' + addr.slice(0, 12) + '…');
ok(await page.locator('.qr svg').count() === 1, 'QR code drawn');
ok(await page.getByText('Send any amount of BTC').count() === 1, 'any amount');
ok(await page.getByText('You can close the app', { exact: false }).count() >= 1, 'says the app can be closed');
await page.locator('.payments', { hasText: 'Waiting for payment' }).waitFor({ timeout: 30000 });
ok(true, 'listed under Payments as waiting (from GET /top_ups)');
shot('topup-address');

// 4. The app is killed, then the payment is sent and confirmed.
adb('shell', 'am', 'force-stop', PKG);
btc('-named', 'sendtoaddress', `address=${addr}`, 'amount=0.001', 'fee_rate=10');
btc('generatetoaddress', '1', btc('getnewaddress'));
ok(true, 'paid 0.001 BTC with the app closed, one block mined');

// 5. Relaunched, the app finds it already (or soon) on the balance.
({ page } = await launch(false));
await page.waitForSelector('#composer', { timeout: 20000 });
await toFunds(page);
const t0 = Date.now();
await page.locator('.payments .ok', { hasText: /^Added \$\d+\.\d\d to your balance$/ }).waitFor({ timeout: 300000 });
const added = await page.locator('.payments .ok').first().textContent();
ok(true, `Payments list says "${added}" after ${Math.round((Date.now() - t0) / 1000)} s`);
await page.getByRole('button', { name: 'Show address' }).first().click();
await page.locator('p.ok', { hasText: 'Added' }).waitFor({ timeout: 15000 });
ok((await page.locator('p.mono').first().textContent()).trim() === addr, 'Show address shows the same address, now Added');
shot('topup-added');
await page.getByText('Done').click();
await page.locator('.bar .icon').first().click(); // back to Account
await page.waitForFunction(() => { const b = document.querySelector('.big'); return b && parseFloat(b.textContent.slice(1)) > 0; }, null, { timeout: 30000 });
ok(true, 'balance after the top-up: ' + await page.locator('.big').textContent());
ok(await page.getByText('Deposit', { exact: true }).count() > 0, 'deposit listed in transactions');

// 6. Blind voucher: a warning that can't be missed, and Continue asks before it pays.
await page.getByText('Add funds').click();
await page.locator('.blind-toggle input').check();
const warn = await page.locator('.warn').textContent();
ok(/open the app again/i.test(warn) && /Uninstalling/.test(warn) && /How it works/.test(warn) && warn.length < 160, 'blind warning is short: ' + warn);
ok(await page.getByText('Check now').count() === 0 && await page.getByText('Details', { exact: true }).count() === 0, 'no Check now or Details buttons');
shot('blind-warning');
await page.getByText('Continue').click();
ok(await page.getByRole('button', { name: 'Yes, continue' }).count() === 1 && await page.locator('.confirm').count() === 1, 'Continue asks to confirm');
ok(await page.locator('p.mono').count() === 0, 'and nothing is ordered yet');
shot('blind-confirm');
await page.getByRole('button', { name: 'Cancel' }).click();
ok(await page.getByRole('button', { name: 'Yes, continue' }).count() === 0, 'Cancel takes the question back');

// 7. Not remembered: the next visit is a top-up again.
await page.locator('.bar .icon').first().click();
await page.getByText('Add funds').click();
ok(!(await page.locator('.blind-toggle input').isChecked()), 'Blind voucher is off again on the next visit');

console.log('all passed');
await device.close();
