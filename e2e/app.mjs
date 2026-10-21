// End-to-end test of the Android app against an Obscurify dev server with the regtest BTCPay stack
// (docker-compose.dev.yml in the server repo). Drives the app's WebView, so build it with
//   OBSCURIFY_API=https://<dev server> DEVTOOLS=1 ./build.sh x86_64
// install it on a running emulator, then: BTC_RPC_PASSWORD=… node e2e/app.mjs
// (BTC_CONTAINER names the regtest bitcoind container, default obscurify-bitcoind-dev.)
// Steps: create an account, fund it with a BTC voucher (regtest), chat with streaming and web
// search, model picker and favorites, chats drawer, account page, restart, sign out and back in.
import { execFileSync } from 'node:child_process';
import { launch, ok, shot, device } from './lib.mjs';

const btc = (...a) => execFileSync('docker', ['exec', process.env.BTC_CONTAINER || 'obscurify-bitcoind-dev', 'bitcoin-cli', '-regtest', '-rpcuser=btcrpc',
  `-rpcpassword=${process.env.BTC_RPC_PASSWORD}`, '-rpcwallet=payer', ...a], { encoding: 'utf8' }).trim();

let { browser, page } = await launch(true);

// 1. A new account, the web's way: Terms checkbox, the number shown once, "I have saved it", then chat.
await page.getByText('Create one').click();
await page.getByRole('button', { name: 'Generate Account Number' }).click();
ok(await page.locator('.err', { hasText: 'Terms of Service' }).count() === 1, 'Terms must be accepted first');
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Generate Account Number' }).click();
let number = (await page.locator('.number').textContent({ timeout: 20000 })).trim();
ok(/^\d{4} \d{4} \d{4} \d{4}$/.test(number), `account created: ${number.slice(0, 4)} …`);
ok(await page.getByRole('button', { name: 'Start Chatting' }).isDisabled(), 'Start Chatting waits for "I have saved my account number"');
shot('number');
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
ok(true, 'chat opens after sign-up');

// 2. Add funds with Bitcoin: the app makes the order, the regtest wallet pays it, the app collects
// vouchers (blind-signed) and adds them.
await page.locator('.bar .icon').first().click();
await page.getByText('Account', { exact: false }).last().click();
await page.getByText('Add funds').click();
await page.getByRole('button', { name: 'Bitcoin' }).click();
await page.locator('input.field').fill('5');
await page.getByText('Continue').click();
const addr = (await page.locator('p.mono').first().textContent({ timeout: 30000 })).trim();
ok(addr.startsWith('bcrt1'), 'BTC address shown: ' + addr.slice(0, 12) + '…');
ok(await page.locator('.qr svg').count() === 1, 'QR code drawn');
shot('btc');
btc('-named', 'sendtoaddress', `address=${addr}`, 'amount=0.001', 'fee_rate=10');
btc('generatetoaddress', '1', btc('getnewaddress'));
await page.getByText('Done').click();
// Vouchers: polled every 10 s; then "Add now" skips the random delay.
await page.waitForSelector('text=waiting to be added', { timeout: 120000 });
ok(true, 'vouchers collected from the paid order');
shot('vouchers');
await page.getByText('Add now').click();
await page.waitForSelector('text=added', { timeout: 30000 });
await page.locator('.bar .icon').first().click(); // back to Account
await page.waitForFunction(() => { const b = document.querySelector('.big'); return b && parseFloat(b.textContent.slice(1)) > 0; }, null, { timeout: 30000 });
const balance = await page.locator('.big').textContent();
ok(parseFloat(balance.slice(1)) > 0, 'balance after redeeming: ' + balance);

// 3. Account page: plan, API key, transactions.
await page.getByText('Show API key').click();
ok((await page.locator('.mono', { hasText: 'sk-obs1_' }).textContent({ timeout: 15000 })).includes('sk-obs1_'), 'API key shown');
ok(await page.getByText('Pay as you go', { exact: false }).count() > 0, 'plans listed');
ok(await page.getByText('Deposit', { exact: true }).count() > 0, 'deposit listed in transactions');
shot('account');
await page.locator('.bar .icon').first().click(); // back to chat

// 4. Model picker: search, star, choose.
await page.locator('.title.model').click();
await page.locator('input.search').fill('llama 3.3 70b');
const row = page.locator('.pick', { hasText: 'Meta: Llama 3.3 70b Instruct' }).first();
await row.locator('button').click();
await row.locator('.grow').click();
ok((await page.locator('.title.model').textContent()).includes('Llama 3.3 70b'), 'model chosen');
await page.locator('.title.model').click();
ok(await page.locator('.chip.on', { hasText: 'Favorites' }).count() === 1 && await page.locator('.pick').count() === 1, 'favorite kept under Favorites');
await page.locator('.drawer .bar .icon').click(); // close the picker

// 5. Chat: a streamed answer, without web search.
await page.locator('.chip', { hasText: 'Web search' }).click(); // on by default, so this turns it off
await page.locator('#composer').fill('Reply with exactly the word: pineapple');
await page.locator('.send').click();
await page.waitForSelector('.msg.assistant .meta', { timeout: 90000 });
const answer = await page.locator('.msg.assistant').last().textContent();
ok(/pineapple/i.test(answer), 'streamed answer: ' + answer.slice(0, 40));
shot('chat');

// 6. Web search: the search chip appears above the answer.
await page.locator('.chip', { hasText: 'Web search' }).click();
await page.locator('#composer').fill('Search the web: what is the latest stable Rust version? One line.');
await page.locator('.send').click();
// Providers sometimes fail a tool call; the app shows the error with Retry, which a user would tap.
for (let attempt = 0; ; attempt++) {
  await page.waitForFunction(() => document.querySelectorAll('.msg').length === 4 && document.querySelector('.msg:last-child .meta'), null, { timeout: 120000 });
  if (await page.locator('.msg.err').count() === 0) break;
  console.log('   provider error, retrying:', (await page.locator('.msg.err p').textContent()).slice(0, 60));
  if (attempt === 2) throw new Error('web search kept failing');
  await page.getByRole('button', { name: 'Retry' }).click();
  await page.waitForFunction(() => !document.querySelector('.msg.err'));
}
const searched = await page.locator('.msg.assistant').last().locator('summary').allTextContents();
console.log('   search chips:', searched);
ok(searched.some(s => s.startsWith('Searched')), 'web search ran');
shot('search');

// 7. Edit the last prompt; the chat is in the drawer, survives a restart, and can be deleted.
await page.getByRole('button', { name: 'Edit' }).click();
ok(await page.locator('.msg').count() === 2 && (await page.locator('#composer').inputValue()).includes('latest stable Rust'), 'edit puts the prompt back');
await page.locator('#composer').fill('');
await browser.close();
({ browser, page } = await launch(false));
await page.waitForSelector('.msg.assistant', { timeout: 15000 });
ok(await page.locator('.msg').count() === 2, 'chat and sign-in survive a restart');
await page.locator('.bar .icon').first().click();
ok(await page.locator('.drawer .item').count() === 1, 'chat listed in the drawer');
ok((await page.locator('.drawer .foot').textContent()).includes('$'), 'balance in the drawer');
shot('drawer');
await page.locator('.drawer .item button').click();
await page.locator('.drawer .item button.danger').click();
ok(await page.locator('.drawer .item').count() === 0, 'chat deleted');
await page.locator('.veil').click({ position: { x: 380, y: 300 } });

// 8. A new number replaces the old one; it's shown once and never again.
await page.locator('.bar .icon').first().click();
await page.locator('.drawer .foot button').click();
ok(await page.getByText('Hidden. We only store a scrambled copy', { exact: false }).count() === 1, 'account number is hidden');
await page.getByRole('button', { name: 'Get a New Number' }).click();
await page.getByRole('button', { name: 'Get a New Number' }).last().click();
const fresh = (await page.locator('.number').textContent({ timeout: 20000 })).trim();
ok(/^\d{4} \d{4} \d{4} \d{4}$/.test(fresh) && fresh !== number, 'new account number shown once');
await page.getByRole('button', { name: "I've Saved It" }).click();
ok(await page.locator('.number').count() === 0, 'and hidden again');

// 9. Sign out; the old number fails, the new one signs in.
await page.getByRole('button', { name: 'Sign out' }).click();
await page.getByRole('button', { name: 'Sign out' }).last().click();
await page.waitForSelector('.login input');
await page.locator('.login input').fill(number);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.login .err', { timeout: 20000 });
ok(true, 'old number refused: ' + await page.locator('.login .err').textContent());
await page.locator('.login input').fill(fresh);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 20000 });
await page.locator('.bar .icon').first().click();
await page.waitForFunction(() => /\$\d/.test(document.querySelector('.drawer .foot')?.textContent || ''), null, { timeout: 20000 });
ok(true, 'signed back in with the account number: ' + await page.locator('.drawer .foot').textContent());

console.log('all passed');
await device.close();
