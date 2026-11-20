// Prod: an old 16-digit number signed in on a browser and on this phone, converted to an account
// code on one of them, then encrypted chat sync between the two, both ways, until both hold the
// same chats. Then a fresh browser and a fresh install sign in with the code and get everything.
//
// Build first with the default (prod) API: DEVTOOLS=1 ./build.sh x86_64 && adb install -r ...
// The test accounts need a little credit for real answers: VOUCHERS names a file with one
// Pro trial voucher per line (minted by the operator). Accounts are made over Tor (prod's sign-up
// limit). Screenshots go to SHOTS (a directory) when it is set.
//   VOUCHERS=/tmp/vouchers.txt SHOTS=/tmp/shots node e2e/prod-sync.mjs
import { execFileSync } from 'node:child_process';
import { randomInt } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { chromium } from 'playwright';
import { launch, adb, sleep, agentSwitches, PKG } from './lib.mjs';
import { accountKey } from '../../obscurify-cli/lib/account.mjs';

const BASE = 'https://obscurify.ai';
const WEB_MODEL = 'google/gemini-2.5-flash-lite';
const PHONE_MODEL = 'Gemini 2.5 Flash Lite';
const CODE = /^[0-9A-HJKMNP-TV-Z]{5}(-[0-9A-HJKMNP-TV-Z]{5}){3}$/;
const vouchers = readFileSync(process.env.VOUCHERS, 'utf8').split('\n').map((s) => s.trim()).filter(Boolean);
const SHOTS = process.env.SHOTS;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });
const files = `/data/user/0/${PKG}/files`;

let failed = 0, step = 0;
const check = (name, cond, detail = '') => { console.log(`${cond ? 'PASS' : 'FAIL'}  ${name}${cond || !detail ? '' : '  -- ' + detail}`); if (!cond) failed++; return cond; };
const phoneShot = (name) => { if (SHOTS) execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/${String(++step).padStart(2, '0')}-phone-${name}.png`]); };
const webShot = async (p, name) => { if (SHOTS) { await p.waitForTimeout(600); await p.screenshot({ path: `${SHOTS}/${String(++step).padStart(2, '0')}-web-${name}.png` }); } };

// --- accounts over Tor -------------------------------------------------------------------------
let n = 0;
const tor = (args) => { n++; return execFileSync('curl', ['-s', '-m', '120', '--socks5-hostname', '127.0.0.1:9050', '-U', `sync${n}${Date.now()}:x`, ...args]).toString(); };
const post = (path, body, headers = []) => tor(['-H', 'Content-Type: application/json', ...headers, '-d', JSON.stringify(body), BASE + path]);
const status = (path, body, headers = []) => tor(['-o', '/dev/null', '-w', '%{http_code}', '-H', 'Content-Type: application/json', ...headers, ...(body ? ['-d', JSON.stringify(body)] : []), BASE + path]);
function fundedNumber() {
  const num = Array.from({ length: 16 }, () => randomInt(10)).join('');
  const made = status('/create_account', { accept_tos: true, account_key: accountKey(num) });
  if (made !== '201') throw new Error('create_account over Tor: ' + made);
  const apiKey = JSON.parse(post('/generate_token', { account_key: accountKey(num) })).token;
  const voucher = vouchers.shift();
  writeFileSync(process.env.VOUCHERS, vouchers.join('\n') + '\n'); // each is good once
  const r = JSON.parse(post('/vouchers/redeem', { vouchers: [voucher] }, ['-H', `Authorization: Bearer ${apiKey}`]));
  if (!(r.credited_usd > 0)) throw new Error('voucher not credited: ' + JSON.stringify(r));
  return { num, apiKey, spaced: num.replace(/(\d{4})(?=\d)/g, '$1 ') };
}

// --- chats in a comparable shape ---------------------------------------------------------------
// Each side keeps chats in its own shape; compare what both must agree on: which chats, and in
// each, the messages' roles and texts in order.
const textOf = (m) => (typeof m.content === 'string' ? m.content : (m.content || []).filter((p) => p.type === 'text').map((p) => p.text).join('')).trim();
const shape = (list) => Object.fromEntries(list.map((c) => [c.id, (c.messages || []).map((m) => `${m.role}:${textOf(m)}`)]));
function same(label, web, phone) {
  const a = shape(web), b = shape(phone);
  const ids = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort();
  const diffs = ids.filter((id) => JSON.stringify(a[id]) !== JSON.stringify(b[id]))
    .map((id) => `${id}: web ${a[id] ? a[id].length + ' msgs' : 'missing'}, phone ${b[id] ? b[id].length + ' msgs' : 'missing'}`);
  check(`${label}: both hold the same ${Object.keys(a).length} chats, message for message`, diffs.length === 0 && Object.keys(a).length === Object.keys(b).length, diffs.join('; '));
  const dupes = (list) => list.length !== new Set(list.map((c) => c.id)).size;
  check(`${label}:   no chat doubled on either side`, !dupes(web) && !dupes(phone));
}
const until = async (what, fn, ms = 120000) => {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) { last = await fn(); if (last) return last; await sleep(2000); }
  throw new Error('timed out waiting for ' + what);
};

// --- the browser -------------------------------------------------------------------------------
const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true });
const webErrors = [];
async function webDevice(name) {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  const p = await ctx.newPage();
  p.name = name;
  p.on('pageerror', (e) => webErrors.push(`${name} page error: ${e.message}`));
  p.on('console', (m) => { if (m.type() === 'error') webErrors.push(`${name} console: ${m.text().slice(0, 200)}`); });
  p.on('response', (r) => { if (r.status() >= 400 && r.url().startsWith(BASE)) webErrors.push(`${name} ${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`); });
  return p;
}
// The Account page's sign-in form, as a person uses it.
async function webSignIn(p, text) {
  await p.goto(BASE + '/account');
  await p.locator('#loginAccountNumber').fill(text);
  await Promise.all([p.waitForNavigation({ timeout: 90000 }).catch(() => null), p.locator('#loginForm button[type=submit]').click()]);
  await p.waitForLoadState('load');
}
const popup = (p) => p.locator('#code-upgrade').waitFor({ state: 'visible', timeout: 30000 }).then(() => true, () => false);
async function webChat(p) {
  if (new URL(p.url()).pathname !== '/') await p.goto(BASE + '/');
  await p.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined' && typeof selectModelFromPicker === 'function');
  await p.evaluate(() => chatStore.ready);
}
async function webPickModel(p) {
  await p.evaluate(() => { localStorage.setItem('obscurify_tools_off', 'true'); localStorage.setItem('obscurify_code_off', 'true'); });
  await p.waitForFunction(() => document.querySelectorAll('#model-select option:not(:disabled)').length > 3, null, { timeout: 60000 });
  const picked = await p.evaluate((ids) => {
    const id = ids.find((m) => document.querySelector(`#model-select option[value="${m}"]:not(:disabled)`));
    if (id) selectModelFromPicker(id);
    return document.querySelector('#model-select').value;
  }, [WEB_MODEL, 'deepseek/deepseek-v4-flash']);
  if (![WEB_MODEL, 'deepseek/deepseek-v4-flash'].includes(picked)) throw new Error('could not pick a cheap model, on ' + picked);
}
async function webSend(p, text) {
  const before = await p.locator('.message.assistant-message').count();
  const input = p.locator('#chat-prompt-input:visible, #prompt-input:visible').first();
  await input.click();
  await input.fill(text);
  await p.locator('#chat-submit-button:visible, #submit-button:visible').first().click();
  await p.waitForFunction((k) => document.querySelectorAll('.message.assistant-message').length > k, before, { timeout: 90000 });
  for (let i = 0; i < 360 && await p.evaluate(() => isGeneratingResponse); i++) await p.waitForTimeout(500);
  await p.waitForTimeout(800);
  return (await p.locator('.message.assistant-message').last().innerText()).trim();
}
// One settled round (waits out a round another tab holds the lock for), then the chats.
const webChats = (p) => p.evaluate(async () => {
  for (let i = 0; i < 300 && navigator.locks; i++) {
    const q = await navigator.locks.query();
    if (!q.held.some((l) => l.name === 'obscurify-chat-sync')) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  await ChatSync.syncNow(); await ChatSync.syncNow();
  return getConversations();
});
const webTitles = (p) => p.locator('.conversation-item-title').allInnerTexts();
const webNew = (p) => p.evaluate(() => startNewConversation());
const webOpen = async (p, id) => { await p.locator(`.conversation-item[data-id="${id}"]`).click(); await p.waitForTimeout(800); };

// --- the phone ---------------------------------------------------------------------------------
const cat = (name) => { try { return execFileSync('adb', ['shell', 'cat', `${files}/${name}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }); } catch { return ''; } };
const json = (name, fallback) => { try { return JSON.parse(cat(name)); } catch { return fallback; } };
const phoneChats = () => json('chats.json', []).map((m) => ({ id: m.id, ...json(`chat-${m.id}.json`, {}) }));
const nudge = async () => { adb('shell', 'input', 'keyevent', 'KEYCODE_HOME'); await sleep(800); adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`); await sleep(2500); };
async function phoneSignIn(page, text) {
  await page.locator('.login input').fill(text);
  await page.locator('.login button.go').click();
  await page.waitForSelector('#composer', { timeout: 90000 });
}
const sheet = (page) => page.locator('#code-upgrade').waitFor({ timeout: 20000 }).then(() => true, () => false);
async function phonePickModel(page) {
  await agentSwitches(page, false);
  await page.locator('.title.model').click();
  await page.locator('input.search').fill(PHONE_MODEL);
  await page.locator('.pick', { hasText: PHONE_MODEL }).first().locator('.grow').click();
}
async function phoneAsk(page, text) {
  const before = await page.locator('.msg.assistant, .msg.err').count();
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForFunction((k) => document.querySelectorAll('.msg.assistant, .msg.err').length > k && !document.querySelector('.send.stop'), before, { timeout: 180000 });
  return (await page.locator('.msg.assistant, .msg.err').last().textContent()).trim();
}
async function drawer(page) {
  if (!(await page.locator('.drawer').isVisible().catch(() => false))) await page.locator('.icon', { hasText: '☰' }).click();
  await sleep(600);
}
const closeDrawer = (page) => page.locator('.drawer .bar .icon').first().click().catch(() => {});
async function phoneOpen(page, title) {
  await drawer(page);
  await page.locator('.drawer .item .grow', { hasText: title }).first().click();
  await sleep(800);
}
async function phoneNew(page) { await page.locator('.bar .icon', { hasText: '＋' }).click(); await sleep(500); }
async function phoneDelete(page, title) {
  await drawer(page);
  const row = page.locator('.drawer .item', { hasText: title }).first();
  await row.locator('button.bare').click();
  await row.locator('button.danger').click();
  await sleep(800);
  await closeDrawer(page);
}
async function phoneConvertInSheet(page) {
  await page.locator('#code-upgrade-go').click();
  await page.locator('#code-upgrade .number').waitFor({ timeout: 30000 });
  const made = (await page.locator('#code-upgrade .number').textContent()).trim();
  phoneShot('new-code');
  await page.locator('#code-upgrade-saved input').check();
  await page.locator('#code-upgrade-convert').click();
  await page.locator('#code-upgrade-done').waitFor({ timeout: 90000 });
  phoneShot('sync-on');
  await page.locator('#code-upgrade-done').click();
  return made;
}
// Rounds on both until they agree (or time runs out), then the final comparison.
async function converge(label, web, ms = 150000) {
  let w = [], ph = [];
  const end = Date.now() + ms;
  while (Date.now() < end) {
    w = await webChats(web);
    ph = phoneChats();
    if (JSON.stringify(shape(w)) === JSON.stringify(shape(ph)) && w.length) break;
    await nudge();
  }
  same(label, w, ph);
  return w;
}

const started = new Date().toISOString();
console.log('started', started);
try {
  // ================================================================================================
  // A. The phone converts; the browser already signed in with the number follows.
  // ================================================================================================
  console.log('\n== A. phone converts, browser follows');
  const A = fundedNumber();
  let { page } = await launch(true);
  await phoneSignIn(page, A.spaced);
  check('A: phone signed in with the number, asked to upgrade', await sheet(page));
  await page.locator('#code-upgrade-later').click();
  await phonePickModel(page);
  const apple = await phoneAsk(page, 'Reply with exactly the word: apple');
  check('A: the phone chats on the number (before any code)', /apple/i.test(apple), apple.slice(0, 60));
  phoneShot('A-number-chat');

  const webA = await webDevice('A-browser');
  await webSignIn(webA, A.spaced);
  check('A: browser signed in with the number on the Account page, asked to upgrade', await popup(webA));
  await webA.locator('#code-upgrade-later').click();
  await webChat(webA);
  await webPickModel(webA);
  const walnut = await webSend(webA, 'Reply with exactly the word: walnut');
  check('A: the browser chats on the number', /walnut/i.test(walnut), walnut.slice(0, 60));
  check('A:   and does not sync yet (numbers do not sync)', !(await webA.evaluate(() => ChatSync.on())));

  // The phone converts from its launch sheet (shown again: as if it never saw it).
  adb('shell', 'rm', '-f', `${files}/code-offer-seen.json`);
  ({ page } = await launch(false));
  check('A: the phone asks on launch', await sheet(page));
  const codeA = await phoneConvertInSheet(page);
  check(`A: the phone converted: ${codeA}`, CODE.test(codeA));
  check('A:   the old API key still works', status('/get_user_balance', null, ['-H', `Authorization: Bearer ${A.apiKey}`]) === '200');

  // The browser: still signed in, asks for the code once, then Combine.
  await webA.goto(BASE + '/');
  check('A: the browser stays signed in', await webA.evaluate(async () => (await fetch('/get_user_balance', { credentials: 'same-origin' })).ok));
  check('A:   and asks for the new code, not to make another', await popup(webA) && (await webA.locator('#code-upgrade-title').innerText()) === 'This account has an account code' && !(await webA.locator('#code-upgrade-go').count()));
  await webShot(webA, 'A-asks-for-code');
  await webA.locator('#code-upgrade-enter').fill(codeA);
  await Promise.all([webA.waitForNavigation({ timeout: 90000 }).catch(() => null), webA.locator('#code-upgrade-signin').click()]);
  await webChat(webA);
  check('A: the browser signed in with the code', await webA.evaluate(() => PrivateSessions.accountKind() === 'code'));
  const askedA = await webA.waitForSelector('#sync-join[open]', { timeout: 30000 }).then(() => true, () => false);
  check('A:   it has a chat of its own and the account has the phone\'s: it asks to combine', askedA, '');
  if (askedA) {
    check('A:   the question names both', /1 chat/.test(await webA.locator('#sync-join-text').innerText()), await webA.locator('#sync-join-text').innerText());
    await webShot(webA, 'A-combine');
    await webA.locator('#sync-join-combine').click();
  }
  await webA.waitForTimeout(1500);
  check('A:   sync is on in the browser', await webA.evaluate(() => ChatSync.on()));
  let w = await converge('A after combining', webA);
  check('A:   that is the phone\'s chat and the browser\'s', w.length === 2 && JSON.stringify(w).includes('apple') && JSON.stringify(w).includes('walnut'));
  await webShot(webA, 'A-both-chats');
  ({ page } = await launch(false, true));
  await drawer(page);
  phoneShot('A-both-chats');
  await closeDrawer(page);

  // Edits both ways: the phone answers in the browser's chat, the browser starts one, the phone
  // deletes its own.
  const walnutId = w.find((c) => JSON.stringify(c).includes('walnut')).id;
  const appleId = w.find((c) => JSON.stringify(c).includes('apple')).id;
  await phoneOpen(page, 'walnut');
  await phonePickModel(page);
  const fig = await phoneAsk(page, 'Reply with exactly the word: fig');
  check('A: the phone adds to the browser\'s chat', /fig/i.test(fig), fig.slice(0, 60));
  await webChat(webA);
  await webNew(webA);
  await webPickModel(webA);
  const cherry = await webSend(webA, 'Reply with exactly the word: cherry');
  check('A: the browser starts a new chat', /cherry/i.test(cherry), cherry.slice(0, 60));
  await phoneDelete(page, 'apple');
  w = await converge('A after edits both ways', webA);
  check('A:   the phone\'s answer is in the browser\'s chat', (w.find((c) => c.id === walnutId)?.messages || []).length === 4);
  check('A:   the browser\'s new chat is on the phone', phoneChats().some((c) => JSON.stringify(c.messages).includes('cherry')));
  check('A:   the chat deleted on the phone is gone from the browser', !w.some((c) => c.id === appleId));
  check('A:   the browser\'s sidebar shows them', (await webTitles(webA)).length === w.length, (await webTitles(webA)).join(' | '));

  // A fresh browser signs in with the code on the Account page: everything is there, no question.
  const webA2 = await webDevice('A-fresh-browser');
  await webSignIn(webA2, codeA);
  check('A: a fresh browser signs in with the code', !(await popup(webA2)) || (await webA2.locator('#code-upgrade-title').innerText()) !== 'This account has an account code');
  await webChat(webA2);
  const fresh = await until('the chats on the fresh browser', async () => { const l = await webChats(webA2); return l.length === w.length && l; });
  check('A:   and gets every chat without being asked to combine', !(await webA2.locator('#sync-join[open]').count()));
  same('A fresh browser vs phone', fresh, phoneChats());
  await webA2.reload();
  await webChat(webA2);
  await webShot(webA2, 'A-fresh-browser');

  // The old number on another new browser: it no longer signs in, and says so.
  const webA3 = await webDevice('A-old-number');
  await webA3.goto(BASE + '/account');
  let alerted = '';
  webA3.on('dialog', async (d) => { alerted = d.message(); await d.dismiss(); });
  await webA3.locator('#loginAccountNumber').fill(A.spaced);
  await webA3.locator('#loginForm button[type=submit]').click();
  await webA3.waitForTimeout(6000);
  const said = alerted || (await webA3.locator('#loginError, .login-error, .error').allInnerTexts().catch(() => [])).join(' ');
  check('A: the old number on a new browser does not sign in', !(await webA3.evaluate(async () => (await fetch('/get_user_balance', { credentials: 'same-origin' })).ok)));
  console.log('      it says: ' + (said || '(nothing found)'));
  await webShot(webA3, 'A-old-number');
  webErrors.splice(0, webErrors.length, ...webErrors.filter((e) => !e.startsWith('A-old-number 401')));
  for (const p of [webA, webA2, webA3]) await p.context().close();

  // ================================================================================================
  // B. The browser converts; the phone already signed in with the number follows.
  // ================================================================================================
  console.log('\n== B. browser converts, phone follows');
  const B = fundedNumber();
  const webB = await webDevice('B-browser');
  await webSignIn(webB, B.spaced);
  check('B: browser signed in with the number, asked', await popup(webB));
  await webB.locator('#code-upgrade-later').click();
  await webChat(webB);
  await webPickModel(webB);
  const plum = await webSend(webB, 'Reply with exactly the word: plum');
  check('B: the browser chats on the number', /plum/i.test(plum), plum.slice(0, 60));

  ({ page } = await launch(true));
  await phoneSignIn(page, B.num);
  await sheet(page);
  await page.locator('#code-upgrade-later').click();
  await phonePickModel(page);
  const pear = await phoneAsk(page, 'Reply with exactly the word: pear');
  check('B: the phone chats on the number', /pear/i.test(pear), pear.slice(0, 60));

  // The browser converts from the Account page's box.
  await webB.goto(BASE + '/account');
  await webB.locator('#syncUpgradeBtn').click();
  await webB.locator('#code-upgrade-code').waitFor({ timeout: 30000 });
  const codeB = (await webB.locator('#code-upgrade-code').innerText()).trim();
  await webB.locator('#code-upgrade-saved').check();
  await webShot(webB, 'B-new-code');
  await webB.locator('#code-upgrade-convert').click();
  await webB.locator('#code-upgrade-done').waitFor({ timeout: 90000 });
  check(`B: the browser converted: ${codeB}`, CODE.test(codeB) && /Encrypted sync is on/.test(await webB.locator('#code-upgrade-title').innerText()));
  await webB.locator('#code-upgrade-done').click();
  check('B:   the old API key still works', status('/get_user_balance', null, ['-H', `Authorization: Bearer ${B.apiKey}`]) === '200');
  await webChat(webB);
  await webChats(webB);

  // The phone: still signed in, asks for the code, then asks to combine.
  ({ page } = await launch(false));
  const enter = await page.locator('#enter-code').waitFor({ timeout: 30000 }).then(() => true, () => false);
  check('B: the phone stays signed in and asks for the new code', enter && (await page.locator('#composer').count()) > 0 && !(await page.locator('#code-upgrade').count()));
  phoneShot('B-asks-for-code');
  await page.locator('#enter-code-input').fill(codeB);
  await page.locator('#enter-code-go').click();
  await page.locator('#enter-code').waitFor({ state: 'detached', timeout: 90000 });
  const joinB = await page.locator('#sync-join').waitFor({ timeout: 60000 }).then(() => true, () => false);
  check('B:   the phone has a chat of its own: it asks to combine', joinB);
  if (joinB) {
    console.log('      it says: ' + (await page.locator('#sync-join-text').textContent()));
    phoneShot('B-combine');
    await page.locator('#sync-join-combine').click();
  }
  w = await converge('B after combining', webB);
  check('B:   that is the browser\'s chat and the phone\'s', w.length === 2 && JSON.stringify(w).includes('plum') && JSON.stringify(w).includes('pear'));

  const plumId = w.find((c) => JSON.stringify(c).includes('plum')).id;
  const pearId = w.find((c) => JSON.stringify(c).includes('pear')).id;
  ({ page } = await launch(false, true));
  await phoneOpen(page, 'plum');
  await phonePickModel(page);
  const date = await phoneAsk(page, 'Reply with exactly the word: date');
  check('B: the phone adds to the browser\'s chat', /date/i.test(date), date.slice(0, 60));
  await phoneNew(page);
  const kiwi = await phoneAsk(page, 'Reply with exactly the word: kiwi');
  check('B: the phone starts a new chat', /kiwi/i.test(kiwi), kiwi.slice(0, 60));
  await webChat(webB);
  await webChats(webB);
  await webOpen(webB, pearId);
  await webPickModel(webB);
  const lemon = await webSend(webB, 'Reply with exactly the word: lemon');
  check('B: the browser adds to the phone\'s chat', /lemon/i.test(lemon), lemon.slice(0, 60));
  // Delete only once the browser has the phone's addition: deleting an older copy than the
  // server's keeps the chat (by design, an edit elsewhere wins over a stale delete).
  const sawDate = await until('the phone\'s message in the browser before deleting', async () => {
    const c = (await webChats(webB)).find((x) => x.id === plumId);
    return c && JSON.stringify(c.messages).includes('exactly the word: date');
  }, 90000).then(() => true, () => false);
  check('B:   the phone\'s addition reached the browser', sawDate);
  await webB.evaluate((id) => deleteConversation(id), plumId);
  w = await converge('B after edits both ways', webB);
  check('B:   the chat deleted in the browser is gone from the phone', !phoneChats().some((c) => c.id === plumId));
  check('B:   the browser\'s answer is in the phone\'s chat', (phoneChats().find((c) => c.id === pearId)?.messages || []).length === 4);
  check('B:   the phone\'s new chat is in the browser', w.some((c) => JSON.stringify(c.messages).includes('kiwi')));
  await webB.reload();
  await webChat(webB);
  await webShot(webB, 'B-browser-after-edits');
  ({ page } = await launch(false, true));
  await drawer(page);
  phoneShot('B-phone-after-edits');

  // A fresh install signs in with the code: everything is there.
  const before = shape(await webChats(webB));
  ({ page } = await launch(true));
  await phoneSignIn(page, codeB);
  check('B: a fresh install signs in with the code, nothing to upgrade', !(await sheet(page)));
  await until('the chats on the fresh install', async () => { await nudge(); return phoneChats().length === Object.keys(before).length; });
  check('B:   and is not asked to combine', !(await page.locator('#sync-join').count()));
  same('B fresh install vs browser', await webChats(webB), phoneChats());
  ({ page } = await launch(false, true));
  await drawer(page);
  phoneShot('B-fresh-install');
  await webB.context().close();
} catch (e) {
  console.log('ERROR ' + (e.stack || e.message));
  failed++;
}

console.log('\n== errors seen by the browsers');
for (const e of webErrors) console.log('  ' + e);
if (!webErrors.length) console.log('  none');
console.log(`\nwindow ${started} .. ${new Date().toISOString()}`);
await browser.close();
console.log(failed ? `${failed} failed` : 'all prod sync checks passed');
process.exit(failed ? 1 : 0);
