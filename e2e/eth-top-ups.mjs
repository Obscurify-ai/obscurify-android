// ETH, USDC and USDT top-ups. Against the server's local two-chain stack
// (obscurify tests/e2e/eth-stack.sh: anvil "Ethereum" on :8545 and "Base" on :8546, ETH at $2,000):
//   OBSCURIFY_API=http://127.0.0.1:8097 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb reverse tcp:8097 tcp:8097 && node e2e/eth-top-ups.mjs
// Steps: sign up; Add funds → Ethereum shows ETH/USDC/USDT and the networks, USDT hides Base; a $10
// USDC top-up on Base shows "Send 10 USDC on Base", the plain address as the QR and that the same
// address works on Ethereum; the app is force-stopped, 10 USDC arrive on Base, and the relaunched app
// has it on the balance and lists nothing in progress.
import { launch, ok, shot, device, adb, PKG } from './lib.mjs';

const BASE_CHAIN = 'http://127.0.0.1:8546';
const PAYER = '0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266'; // anvil account 0, unlocked
const USDC = '0x5FbDB2315678afecb367f032d93F642f64180aa3';
async function rpc(method, params) {
  const r = await fetch(BASE_CHAIN, { method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
  const body = await r.json();
  if (body.error) throw new Error(JSON.stringify(body.error));
  return body.result;
}
const toFunds = async page => {
  await page.locator('.bar .icon').first().click();
  await page.locator('.drawer .foot button').click();
  await page.getByText('Add funds').click();
  await page.waitForSelector('.blind-toggle');
};

let { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Create Account' }).click();
await page.locator('.number').waitFor({ timeout: 20000 });
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
ok(true, 'signed up');

await toFunds(page);
await page.getByRole('button', { name: 'USDT/ETH' }).first().click();
const tokens = await page.locator('.evm-tokens .chip').allTextContents();
ok(tokens.join(' ') === 'ETH USDC USDT', 'tokens: ' + tokens.join(' '));
let networks = await page.locator('.evm-networks .chip').allTextContents();
ok(networks.join(' ') === 'Ethereum Base Arbitrum Optimism', 'networks for ETH (none on BNB Chain or Tron): ' + networks.join(' '));
await page.locator('.evm-tokens .chip', { hasText: 'USDT' }).click();
networks = await page.locator('.evm-networks .chip').allTextContents();
ok(networks.join(' ') === 'Ethereum Arbitrum Optimism BNB Chain Tron', 'USDT: no Base, adds BNB Chain and Tron: ' + networks.join(' '));
shot('eth-choose');

await page.locator('.evm-tokens .chip', { hasText: 'USDC' }).click();
await page.locator('.evm-networks .chip', { hasText: 'Base' }).click();
await page.locator('input.field').fill('10');
await page.getByText('Continue').click();
const addr = (await page.locator('p.mono').first().textContent({ timeout: 30000 })).trim();
ok(/^0x[0-9a-fA-F]{40}$/.test(addr), 'address shown: ' + addr);
ok(await page.getByText('Send 10 USDC on Base').count() === 1, 'says how much, of what, on which network');
ok(await page.getByText('Same address works on Ethereum.').count() === 1, 'says the address works on Ethereum too');
ok(await page.locator('.qr svg').count() === 1, 'QR code drawn');
await page.locator('.payments', { hasText: 'USDC on Base' }).waitFor({ timeout: 30000 });
ok(true, 'listed under In progress as USDC on Base');
shot('eth-address');

adb('shell', 'am', 'force-stop', PKG);
await rpc('eth_sendTransaction', [{ from: PAYER, to: USDC,
  data: '0x40c10f19' + addr.slice(2).toLowerCase().padStart(64, '0') + (10_000_000).toString(16).padStart(64, '0') }]);
await rpc('anvil_mine', ['0x2']);
ok(true, 'paid 10 USDC on Base with the app closed');

({ page } = await launch(false));
await page.waitForSelector('#composer', { timeout: 20000 });
await toFunds(page);
await page.waitForFunction(() => !document.querySelector('.payments'), null, { timeout: 120000 });
ok(true, 'paid: no longer listed under In progress');
await page.locator('.bar .icon').first().click();
await page.waitForFunction(() => { const b = document.querySelector('.big'); return b && parseFloat(b.textContent.slice(1)) >= 10; }, null, { timeout: 30000 });
ok(true, 'balance after the top-up: ' + await page.locator('.big').textContent());
shot('eth-added');

console.log('all passed');
await device.close();
