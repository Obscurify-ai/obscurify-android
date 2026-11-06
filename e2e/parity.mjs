// Features the app shares with the website: guest chat, the Tools switch, the provider filter,
// raw view and editing any answer, chat times, links, plan confirmation, amount presets, and
// deleting the account. Test APK against a server, then: ACCOUNT=<funded number> node e2e/parity.mjs
import { launch, ok, sleep } from './lib.mjs';

const answered = (page, n) => page.waitForFunction((n) => document.querySelectorAll('.msg.assistant').length >= n && !document.querySelector('.send.stop'), n, { timeout: 150000 });

// 1. Guest: no account, one free model.
let { page } = await launch(true);
await page.getByRole('button', { name: 'Chat as a guest' }).click();
await page.waitForSelector('#composer');
await page.waitForSelector('.tools-switch.on', { timeout: 20000 });
ok(true, 'a guest has tools, and the app says so');
await page.locator('#composer').fill('Use run_code with Python to compute 1234*5678 and tell me the result.');
await page.locator('.send').click();
await answered(page, 1);
const firstGuest = await page.locator('.msg').last().textContent();
ok(await page.locator('.run').count() >= 1 && !firstGuest.includes('<function'), 'a guest runs code: ' + firstGuest.slice(0, 160).replace(/\s+/g, ' '));
ok((await page.locator('.msg.assistant').last().textContent()).replace(/,/g, '').includes('7006652'), 'a guest gets the computed answer');
await page.locator('.bar .icon').last().click();
await page.locator('#composer').fill('Use run_code with Python: result = await tools.web_search("current year") and print it. Then say DONE.');
await page.locator('.send').click();
await answered(page, 1);
const guestRun = await page.locator('.msg.assistant').last().textContent();
ok(!/not available|origin|forbidden/i.test(guestRun) && await page.locator('.run').count() >= 1, 'a guest can search the web: ' + guestRun.slice(0, 90).replace(/\s+/g, ' '));
await sleep(1500);
ok(await page.locator('#composer').count() === 1, 'a guest stays in the chat (no session-ended bounce)');
await page.locator('.bar .icon').first().click();
ok(await page.locator('.foot button', { hasText: 'Sign in' }).count() === 1, 'the drawer offers Sign in');
const links = await page.locator('.foot .links a').allTextContents();
ok(['All tools', 'Model cards', 'Tor', 'Privacy', 'Terms', 'Canary', 'Support'].every((l) => links.includes(l)), 'drawer links: ' + links.join(', '));
ok((await page.locator('.item .when').first().textContent()).includes('ago') || (await page.locator('.item .when').first().textContent()).includes('just now'), 'chats show when they were last used');
await page.locator('.veil').click({ position: { x: 380, y: 400 } });
await page.locator('.title.model').click();
await page.waitForSelector('.login');
ok(await page.getByText('Create Account').count() >= 1, 'a guest picking a model is asked to create an account');

// 2. A new account: optional referral code, 18+; deleting it from the app.
await page.locator('.check input').check();
ok((await page.locator('.check').textContent()).includes("I'm 18 or older"), 'sign-up says 18+');
ok(await page.locator('input[placeholder^="Referral code"]').count() === 1, 'sign-up takes a referral code');
await page.getByRole('button', { name: 'Generate Account Number' }).click();
const fresh = (await page.locator('.number').textContent({ timeout: 20000 })).trim();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
await page.locator('.bar .icon').first().click();
await page.locator('.foot button', { hasText: 'Account' }).click();
await page.waitForSelector('.big');
await page.getByRole('button', { name: 'Delete account…' }).click();
await page.getByRole('button', { name: 'Delete for good' }).click();
await page.waitForSelector('.login', { timeout: 20000 });
await page.locator('.login input').fill(fresh);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.login .err', { timeout: 20000 });
ok(true, 'account deleted from the app; its number no longer signs in');

// 3. Signed in: Tools switch, picker, raw, edit.
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });
await page.waitForSelector('.tools-switch', { timeout: 20000 });
ok(await page.locator('.tools-switch.on').count() === 1 && (await page.locator('.tools-switch').getAttribute('aria-checked')) === 'true', 'Tools is on by default, and says so');
// It sits in the message line, between Attach and the box, so it costs no row of its own.
{
  const [sw, box, attach, send] = await Promise.all(['.tools-switch', '#composer', '.attach', '.send'].map((s) => page.locator(s).boundingBox()));
  ok(sw.x >= attach.x + attach.width && sw.x + sw.width <= box.x && sw.y >= box.y - 1 && sw.y + sw.height <= send.y + send.height + 1, 'Tools is in the message line: ' + JSON.stringify({ sw, box }));
  // Attach, Tools and Send are centred on the text, whatever the phone's font size.
  const lh = parseFloat(await page.locator('#composer').evaluate((e) => getComputedStyle(e).lineHeight));
  const text = box.y + box.height - 11 - lh / 2, mids = [attach, sw, send].map((b) => b.y + b.height / 2);
  ok(mids.every((m) => Math.abs(m - text) < 1.5), 'Attach, Tools and Send line up with the message text: ' + JSON.stringify({ text, mids }));
}
ok(await page.locator('.attach').count() === 1 && await page.locator('.chip.file, .chip.doc').count() === 0, 'one attach button, no Image or File');
await page.locator('.title.model').click();
await page.waitForSelector('.providers .chip', { timeout: 20000 });
const all = await page.locator('.pick').count();
await page.locator('.providers .chip', { hasText: /^Anthropic$/ }).click();
await sleep(500);
const names = await page.locator('.pick .grow > div:first-child').allTextContents();
ok(names.length > 0 && names.length < all && names.every((n) => n.startsWith('Anthropic')), `provider filter: ${all} -> ${names.length}`);
ok((await page.locator('.pick .price').first().textContent()).includes('context'), 'models show their context size');
await page.locator('.providers .chip', { hasText: /^Anthropic$/ }).click();
await page.locator('input.search').fill('Llama 3.3 70b Instruct');
await page.locator('.pick', { hasText: 'Meta: Llama 3.3 70b Instruct' }).first().locator('.grow').click();
await page.locator('.tools-switch').click();
ok(await page.locator('.tools-switch.on').count() === 0 && (await page.locator('.tools-switch').getAttribute('aria-checked')) === 'false', 'Tools switches off');
for (const [i, w] of [[1, 'mango'], [2, 'papaya']]) {
  await page.locator('#composer').fill(`Reply with exactly the word: ${w}`);
  await page.locator('.send').click();
  await answered(page, i);
}
ok(await page.locator('.run').count() === 0, 'with Tools off, no code runs');
await page.locator('.msg.assistant').first().locator('.meta button', { hasText: 'Raw' }).click();
ok(await page.locator('.msg.assistant').first().locator('pre.raw').count() === 1, 'an answer can be shown raw');
await page.locator('.msg.assistant').first().locator('.meta button', { hasText: 'Edit' }).click();
await sleep(500);
ok(await page.locator('.msg').count() === 0 && (await page.locator('#composer').inputValue()).includes('mango'), 'Edit on an earlier answer goes back to its question');
await page.locator('#composer').fill('');
await page.locator('.tools-switch').click();

// 4. Account: a plan change asks first; Add funds has amount presets.
await page.locator('.bar .icon').first().click();
await page.locator('.foot button', { hasText: 'Account' }).click();
await page.waitForSelector('.big');
await page.waitForSelector('button:has-text("Upgrade"), button:has-text("Switch")', { timeout: 20000 });
const before = await page.locator('.big').textContent();
await page.locator('.section button', { hasText: /^(Upgrade|Switch)$/ }).last().click();
await sleep(400);
const ask = await page.locator('.section .muted', { hasText: /costs|Pay \$|Switch to/ }).first().textContent();
ok(/costs|Pay \$|Switch to/.test(ask), 'a plan change asks first: ' + ask);
ok(await page.locator('.big').textContent() === before, 'nothing charged yet');
await page.getByRole('button', { name: 'Cancel' }).first().click();
await page.locator('.section button.go', { hasText: 'Add funds' }).first().click();
await page.locator('.toggles .chip', { hasText: '$50' }).click();
ok(await page.locator('input.field').first().inputValue() === '50', 'amount presets fill the amount');
console.log('all passed');
process.exit(0);
