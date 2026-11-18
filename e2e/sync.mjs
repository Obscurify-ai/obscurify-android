// Encrypted chat sync between the phone and the website (src/sync.rs; on the website
// static/js/chat/chat-sync.js and src/api/sync.rs). One account number on both: a chat made on the
// website shows up on the phone with its image and folder, the phone's answers go back, a chat made
// on the phone shows up on the website, edits and deletes go both ways, favorites follow, and
// nothing readable or linkable leaves the phone. A desktop Chromium is the other device.
//
// The app talks to a recorder on :8232 that forwards to the website's dev server (sync on), so the
// test sees exactly what left the phone:
//   OBSCURIFY_API=http://127.0.0.1:8232 DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
//   adb root; adb reverse tcp:8232 tcp:8232
//   UPSTREAM_PORT=8097 DB=obscurify_sync SECRET_FILE=~/Work/obscurify-sync/local/anon_keys/account_secret node sync.mjs
import http from 'node:http';
import { randomBytes } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const DB = process.env.DB || 'obscurify_sync';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const seen = [];
const recorder = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    const entry = { method: req.method, path: req.url, headers: req.headers, body };
    seen.push(entry);
    const up = http.request({ ...UPSTREAM, method: req.method, path: req.url, headers: { ...req.headers, host: `${UPSTREAM.host}:${UPSTREAM.port}` } }, (r) => {
      entry.status = r.statusCode;
      res.writeHead(r.statusCode, r.headers);
      r.pipe(res);
    });
    up.on('error', () => res.destroy());
    up.end(body);
  });
});
await new Promise((r) => recorder.listen(8232, '127.0.0.1', r));
adb('reverse', 'tcp:8232', 'tcp:8232');
const files = `/data/user/0/${PKG}/files`;
const cat = (name) => { try { return execFileSync('adb', ['shell', 'cat', `${files}/${name}`], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }); } catch { return ''; } };
const json = (name, fallback) => { try { return JSON.parse(cat(name)); } catch { return fallback; } };
const until = async (what, fn, ms = 90000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const v = await fn(); if (v) return v; await sleep(1500); }
  throw new Error('FAILED: timed out waiting for ' + what);
};
// The phone syncs every 30 s while on screen and 1.5 s after a change; going to the background and
// back starts a round at once.
const nudge = async () => { adb('shell', 'input', 'keyevent', 'KEYCODE_HOME'); await sleep(800); adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`); await sleep(1500); };

// A real PNG (the server checks image bytes), random pixels so it's big enough to go up as its own record.
function png(w, h) {
  const crcTable = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = (b) => { let c = 0xffffffff; for (const x of b) c = crcTable[(c ^ x) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (type, data) => { const t = Buffer.concat([Buffer.from(type), data]); const len = Buffer.alloc(4); len.writeUInt32BE(data.length); const c = Buffer.alloc(4); c.writeUInt32BE(crc(t)); return Buffer.concat([len, t, c]); };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  const rows = Buffer.concat(Array.from({ length: h }, () => Buffer.concat([Buffer.from([0]), randomBytes(w * 3)])));
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(rows)), chunk('IEND', Buffer.alloc(0))]);
}
const IMAGE = 'data:image/png;base64,' + png(40, 40).toString('base64');
const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true });
let web;

try {
  // 1. Sign up on the phone and fund the account (a deposit: there's no payment on dev).
  let { page } = await launch(true);
  await page.getByText('Create one').click();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Generate Account Number' }).click();
  const number = (await page.locator('.number').textContent({ timeout: 30000 })).trim();
  const digits = number.replace(/\D/g, '');
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  const key = execFileSync(process.env.SESSIONS_BIN || `${process.env.HOME}/Work/unlinkable-inference/target/release/unlinkable-inference`, ['login-key'], { input: digits }).toString().trim();
  const secretFile = (process.env.SECRET_FILE || `${process.env.HOME}/Work/obscurify-sync/local/anon_keys/account_secret`).replace(/^~/, process.env.HOME);
  const hash = execFileSync('python3', ['-c', 'import hmac,hashlib,sys; print(hmac.new(bytes.fromhex(open(sys.argv[1]).read().strip()), b"obscurify-login-key-v1:"+sys.argv[2].encode(), hashlib.sha256).hexdigest())', secretFile, key]).toString().trim();
  const userId = sql(`SELECT id FROM users WHERE account_number_hash = '${hash}'`);
  ok(/^[0-9a-f-]{36}$/.test(userId), 'signed up on the phone');
  sql(`UPDATE users SET balance = balance + 5 WHERE id = '${userId}'; INSERT INTO transactions (user_id, amount, transaction_type) VALUES ('${userId}', 5, 'deposit')`);

  // 2. The same number on the website: a chat with an image, in a folder, and favorites.
  const ctx = await browser.newContext();
  web = await ctx.newPage();
  await web.goto(WEB + '/');
  await web.waitForFunction(() => window.PrivateSessions && window.ChatSync && typeof chatStore !== 'undefined');
  await web.evaluate(async (n) => {
    localStorage.setItem('obscurify_private_setup_later', '1');
    localStorage.setItem('obscurify_private_off', '1');
    await PrivateSessions.keys(n);
  }, digits);
  await web.reload();
  await web.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined');
  const WEB_ID = 'conv-' + Date.now() + '-e2e';
  const webChat = {
    id: WEB_ID, title: 'Made on the website', folder: 'Work', model: 'deepseek/deepseek-v4-flash', createdAt: Date.now(), updatedAt: Date.now(),
    messages: [
      { role: 'user', content: [{ type: 'text', text: 'What is in this picture?' }, { type: 'image_url', image_url: { url: IMAGE } }] },
      { role: 'assistant', content: 'A test pattern.', display: 'A test pattern.', model: 'deepseek/deepseek-v4-flash', modelLabel: 'deepseek-v4-flash' },
    ],
  };
  await web.evaluate(async (c) => {
    await chatStore.ready;
    await chatStore.save(chatStore.all().concat([c]));
    saveFavoriteModels(['deepseek/deepseek-v4-flash', 'z-ai/glm-5']);
    await ChatSync.syncNow(); await ChatSync.syncNow();
  }, webChat);
  const webSync = async () => web.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); });

  // 3. It reaches the phone.
  await nudge();
  const list = await until('the website\'s chat on the phone', () => json('chats.json', []).find((m) => m.id === WEB_ID));
  ok(list.title === 'Made on the website', 'the website\'s chat is on the phone');
  const phoneChat = json(`chat-${WEB_ID}.json`, {});
  ok(phoneChat.messages && phoneChat.messages[0].images && phoneChat.messages[0].images[0] === IMAGE, 'with its image, intact');
  ok(phoneChat.messages[0].content === 'What is in this picture?', 'in the app\'s own shape');
  ok(phoneChat.folder === 'Work', 'its folder rides along (the app has no folders)');
  ok(JSON.stringify(json('settings.json', {}).favorites) === JSON.stringify(['deepseek/deepseek-v4-flash', 'z-ai/glm-5']), 'favorites came over');
  page = (await launch(false, true)).page;
  await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .item', { hasText: 'Made on the website' }).waitFor({ timeout: 15000 });
  shot('sync-drawer');
  ok(true, 'the drawer lists it');

  // 4. Answer in that chat on the phone: the website gets it, with its own fields kept.
  await page.locator('.drawer .item .grow', { hasText: 'Made on the website' }).click();
  await page.waitForSelector('.msg img');
  ok(true, 'the image shows in the chat');
  await agentSwitches(page, false);
  // A model that takes images (the chat has one).
  await page.locator('.title.model').click();
  await page.locator('input.search').fill('Gemini 2.5 Flash Lite');
  await page.locator('.pick', { hasText: 'Gemini 2.5 Flash Lite' }).first().locator('.grow').click();
  const ask = async (text) => {
    const before = await page.locator('.msg.assistant, .msg.err').count();
    await page.locator('#composer').fill(text);
    await sleep(300);
    await page.locator('.send').click();
    await page.waitForFunction((n) => document.querySelectorAll('.msg.assistant, .msg.err').length > n && !document.querySelector('.send.stop'), before, { timeout: 180000 });
    return (await page.locator('.msg.assistant, .msg.err').last().textContent()).trim();
  };
  const pine = await ask('Reply with exactly the word: pineapple');
  ok(/pineapple/i.test(pine), 'the phone answered in the website\'s chat: ' + pine.slice(0, 80));
  const back = await until('the phone\'s answer on the website', async () => {
    const c = (await webSync()).find((x) => x.id === WEB_ID);
    return c && c.messages.length === 4 && c;
  });
  ok(/pineapple/i.test(JSON.stringify(back.messages[3])), 'the website has the phone\'s answer');
  ok(JSON.stringify(back.messages[0]) === JSON.stringify(webChat.messages[0]), 'the website\'s first message came back exactly as it was');
  ok(back.messages[1].modelLabel === 'deepseek-v4-flash' && back.messages[1].display === 'A test pattern.', 'fields only the website uses came back');
  ok(back.folder === 'Work', 'so did the folder');
  ok(Array.isArray(back.messages[2].content) && back.messages[2].content[0].text.includes('pineapple'), 'the phone\'s message is in the website\'s shape');

  // 5. A chat started on the phone shows up on the website.
  await page.locator('.bar .icon', { hasText: '＋' }).click();
  await sleep(500);
  const mangoAnswer = await ask('Reply with exactly the word: mango');
  ok(/mango/i.test(mangoAnswer), 'a new chat on the phone: ' + mangoAnswer.slice(0, 80));
  const mango = await until('the phone\'s chat on the website', async () => (await webSync()).find((c) => JSON.stringify(c.messages).includes('exactly the word: mango')));
  ok(mango.messages.length === 2 && typeof mango.updatedAt === 'number' && mango.createdAt, 'the phone\'s chat is on the website');

  // 6. The website adds to the phone's open chat: the phone shows it.
  await web.evaluate(async (id) => {
    const list = chatStore.all();
    const c = list.find((x) => x.id === id);
    c.messages.push({ role: 'user', content: [{ type: 'text', text: 'Added on the website' }] });
    c.updatedAt = Date.now();
    await chatStore.save(list);
    await ChatSync.syncNow(); await ChatSync.syncNow();
  }, mango.id);
  await nudge();
  page = (await launch(false, true)).page;
  await until('the website\'s message in the phone\'s chat', async () => (json(`chat-${mango.id}.json`, { messages: [] }).messages.length === 3));
  await page.waitForFunction(() => document.body.innerText.includes('Added on the website'), null, { timeout: 30000 });
  ok(true, 'the open chat on the phone shows the website\'s message');

  // 7. Deleting on the phone deletes on the website.
  await page.locator('.icon', { hasText: '☰' }).click();
  const row = page.locator('.drawer .item', { hasText: 'Made on the website' });
  await row.locator('button.bare').click();
  await row.locator('button.danger').click();
  await until('the delete on the website', async () => !(await webSync()).some((c) => c.id === WEB_ID));
  ok(true, 'a chat deleted on the phone goes from the website');
  await page.locator('.drawer .bar .icon').first().click().catch(() => {});

  // 8. What left the phone: only the sync token, no cookie, nothing readable.
  const syncs = seen.filter((r) => r.path.startsWith('/sync/v1'));
  ok(syncs.length > 3, `the phone synced (${syncs.length} requests)`);
  ok(syncs.every((r) => /^Bearer sync1_[0-9a-f]{64}$/.test(r.headers.authorization || '')), 'every sync request carries only the sync token');
  ok(syncs.every((r) => !r.headers.cookie), 'and no cookie');
  ok(!syncs.some((r) => r.body.includes('pineapple') || r.body.includes('mango') || r.body.includes('Made on the website')), 'no chat text in the clear');
  ok(!syncs.some((r) => r.body.includes(IMAGE.slice(40, 120))), 'no image in the clear');
  ok(syncs.filter((r) => r.method === 'PUT').every((r) => /\/sync\/v1\/records\/[0-9a-f]{32}$/.test(r.path)), 'record ids say nothing');

  // 9. The switch: off on the phone deletes the synced copy, and the website stops too.
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#sync-switch').waitFor({ timeout: 15000 });
  ok(await page.locator('#sync-on').isChecked(), 'Account shows Sync chats between devices, on');
  await page.locator('#sync-switch').scrollIntoViewIfNeeded();
  shot('sync-account');
  await page.locator('#sync-on').uncheck();
  await page.locator('#sync-off-confirm').click();
  await until('sync off on the phone', async () => cat('sync-off.json').trim() === 'true');
  ok(!cat('sync-state.json'), 'the phone forgot its sync records');
  await webSync();
  ok(await web.evaluate(() => !ChatSync.on()), 'the website stopped syncing too');
  ok((await web.evaluate(() => getConversations())).some((c) => c.id === mango.id), 'and kept its chats');
  ok(json('chats.json', []).some((m) => m.id === mango.id), 'the phone kept its chats');
  console.log('\nall sync checks passed');
} finally {
  await browser.close();
  recorder.close();
}
