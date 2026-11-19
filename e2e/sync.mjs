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
import { randomBytes, createHmac, createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright';
import { launch, adb, ok, shot, sleep, agentSwitches, PKG } from './lib.mjs';

const UPSTREAM = { host: '127.0.0.1', port: +(process.env.UPSTREAM_PORT || 8097) };
const WEB = `http://${UPSTREAM.host}:${UPSTREAM.port}`;
const DB = process.env.DB || 'obscurify_sync';
const sql = (q) => execFileSync('docker', ['exec', '-i', 'obscurify-postgres-dev', 'psql', '-U', 'obscurify', '-d', DB, '-tAX', '-c', q]).toString().trim();
const seen = [];
let blockSync = false; // the phone "offline" for sync (503, retry in 1 s)
const recorder = http.createServer((req, res) => {
  if (blockSync && req.url.startsWith('/sync/')) { res.writeHead(503, { 'Retry-After': '1' }); res.end(); return; }
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
  await page.getByRole('button', { name: 'Create Account' }).click();
  await page.locator('.number').waitFor({ timeout: 30000 });
  // An account code (only accounts with a code sync); "digits" is what signs in.
  const digits = (await page.locator('.number').textContent()).trim();
  await page.locator('.check input').check();
  await page.getByRole('button', { name: 'Start Chatting' }).click();
  await page.waitForSelector('#composer');
  const key = execFileSync(process.env.SESSIONS_BIN || `${process.env.HOME}/Work/unlinkable-inference-words/target/release/unlinkable-inference`, ['login-key'], { input: digits }).toString().trim();
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
  ok(await page.locator('#sync-on').isChecked(), 'Account shows Sync encrypted chats between devices, on');
  await page.locator('#sync-switch').scrollIntoViewIfNeeded();
  shot('sync-account');
  await page.locator('#sync-on').uncheck();
  await page.locator('#sync-off-confirm').click();
  await until('sync off on the phone', async () => cat('sync-off.json').trim() === 'true');
  const left = json('sync-state.json', {});
  ok(Object.values(left.convs || {}).every((r) => !r.rev && !r.dirty) && !left.epoch && !left.after && left.owner, 'the phone forgot its sync records (it keeps which account its chats are from, and their titles as last synced)');
  await webSync();
  ok(await web.evaluate(() => !ChatSync.on()), 'the website stopped syncing too');
  ok((await web.evaluate(() => getConversations())).some((c) => c.id === mango.id), 'and kept its chats');
  ok(json('chats.json', []).some((m) => m.id === mango.id), 'the phone kept its chats');

  // 10. Back on, then a new account number: the chats move to its store; the website, still on
  // the old number, stops.
  await page.locator('#sync-on').check();
  await until('sync back on', async () => !cat('sync-off.json').includes('true') && cat('sync-state.json').includes('"epoch":"'));
  await web.evaluate(() => ChatSync.setOn(true));
  await webSync();
  await page.locator('#rotate-words').click();
  await page.locator('#new-words .number').waitFor({ timeout: 30000 });
  const fresh = (await page.locator('#new-words .number').textContent()).trim();
  await page.locator('#words-saved input').check();
  await page.locator('#switch-words').click();
  await page.locator('#new-words', { hasText: 'Done' }).waitFor({ timeout: 60000 });
  ok(/^[0-9A-Z]{5}(-[0-9A-Z]{5}){3}$/.test(fresh) && fresh !== digits, 'the phone got a new account code');
  await until('the phone moved to the new store', async () => cat('sync-seed.json').length > 60 && json('sync-state.json', {}).epoch);
  await webSync();
  ok(await web.evaluate(() => !ChatSync.on()), 'the website on the old number stopped syncing');
  const ctx2 = await browser.newContext();
  const web2 = await ctx2.newPage();
  await web2.goto(WEB + '/');
  await web2.waitForFunction(() => window.PrivateSessions && window.ChatSync && typeof chatStore !== 'undefined');
  const seed2 = await web2.evaluate((n) => PrivateSessions.seedOf(n), fresh);
  await web2.evaluate((s) => { localStorage.setItem('obscurify_private_off', '1'); localStorage.setItem('obscurify_account_kind', 'code'); localStorage.setItem('obscurify_sync_seed', s); }, seed2);
  await web2.reload();
  await web2.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined');
  const moved = await until('the phone\'s chats under the new number', async () => (await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); })).find((c) => c.id === mango.id));
  ok(moved.messages.length === 3, 'a browser with the new number gets the phone\'s chats');

  // 11. Wipe on the phone: its chats and the synced copy go, and nothing comes back.
  await page.locator('.bar .icon').first().click().catch(() => {});
  await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('#wipe').click();
  await page.locator('#wipe-yes').click();
  await until('the wipe', async () => json('chats.json', []).length === 0);
  await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); });
  ok(await web2.evaluate(() => !ChatSync.on()), 'Wipe deleted the synced copy (the browser on that number stopped)');
  ok(await web2.evaluate(() => getConversations().length > 0), '  and the browser kept its chats');
  await nudge();
  await sleep(5000);
  ok(json('chats.json', []).length === 0, 'nothing came back to the phone');
  ok(!cat('sync-state.json').includes('mango'), '  and no wiped chat\'s title is left in the sync records');

  // 12. Joining with chats on both sides: the phone asks first. A phone that has chats from before
  // it synced (an app from before sync, here: chats made with sync off, then the off flag gone)
  // meets an account whose synced copy has the browser's chats.
  if (!(await page.locator('.drawer .foot .account').isVisible())) await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .foot .account').click();
  await page.locator('#sync-on').uncheck();
  await page.locator('#sync-off-confirm').click(); // joined: switching off deletes the synced copy, so it asks
  await until('sync off', async () => cat('sync-off.json').trim() === 'true');
  await page.locator('.bar .icon').first().click().catch(() => {});
  await page.locator('.bar .icon', { hasText: '＋' }).click().catch(() => {});
  await sleep(500);
  const kiwi = await ask('Reply with exactly the word: kiwi');
  ok(/kiwi/i.test(kiwi), 'a chat made on the phone with sync off: ' + kiwi.slice(0, 40));
  const kiwiId = json('chats.json', []).find((m) => /kiwi/i.test(m.title || '') || /kiwi/i.test(cat(`chat-${m.id}.json`))).id;
  await web2.evaluate(() => ChatSync.setOn(true));
  const webChats = await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations().map((c) => c.id); });
  ok(webChats.length > 0 && !webChats.includes(kiwiId), `the browser's ${webChats.length} chat(s) are synced, the phone's isn't`);
  execFileSync('adb', ['shell', 'rm', '-f', `${files}/sync-off.json`, `${files}/sync-joined.json`]);
  // The store's name on the server: SHA-256 of the token made from the number's seed (FORMAT).
  const lp = (parts) => Buffer.concat(parts.flatMap((x) => { const b = Buffer.from(x); const n = Buffer.alloc(4); n.writeUInt32BE(b.length); return [n, b]; }));
  const storeHash = createHash('sha256').update('sync1_' + createHmac('sha256', Buffer.from(seed2, 'hex')).update(lp(['chat-sync', 'token'])).digest('hex')).digest('hex');
  const epochBefore = sql(`SELECT epoch FROM sync_stores WHERE store_hash = '${storeHash}'`);
  ok(epochBefore.length === 16, '(the browser\'s synced copy is on the server)');
  const writesBefore = seen.filter((r) => r.path.startsWith('/sync/') && r.method !== 'GET').length;
  await nudge();
  page = (await launch(false, true)).page;
  await page.locator('#sync-join').waitFor({ timeout: 60000 });
  const askText = await page.locator('#sync-join-text').textContent();
  ok(askText.includes('This phone has 1 chat that isn\'t synced') && askText.includes(`${webChats.length} chat`), 'the phone asks before combining: ' + askText);
  shot('sync-join');
  ok(!json('chats.json', []).some((m) => webChats.includes(m.id)), '  until answered, nothing comes in');
  ok(seen.filter((r) => r.path.startsWith('/sync/') && r.method !== 'GET').length === writesBefore, '  and nothing goes out');

  // "Not on this phone": sync off here only; the browser carries on.
  await page.locator('#sync-join-no').click();
  await until('sync off after "Not on this phone"', async () => cat('sync-off.json').trim() === 'true');
  await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); });
  ok(await web2.evaluate(() => ChatSync.on()) && sql(`SELECT count(*) FROM sync_stores WHERE epoch = '${epochBefore}'`) === '1', '"Not on this phone": the synced copy and the browser carry on');
  ok(!json('chats.json', []).some((m) => webChats.includes(m.id)), '  and the phone keeps only its own chats');

  // Asked again (the app updated again, say), and this time Combine.
  execFileSync('adb', ['shell', 'rm', '-f', `${files}/sync-off.json`]);
  await nudge();
  page = (await launch(false, true)).page;
  await page.locator('#sync-join').waitFor({ timeout: 60000 });
  await page.locator('#sync-join-combine').click();
  await until('the browser\'s chats on the phone', async () => webChats.every((id) => json('chats.json', []).some((m) => m.id === id)));
  const union = await until('the phone\'s chat on the browser', async () => {
    const list = await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); });
    return list.some((c) => c.id === kiwiId) && list;
  });
  ok(union.length === webChats.length + 1, `Combine: both have all ${union.length} chats`);
  ok(json('chats.json', []).length === union.length, '  no chat doubled on the phone');

  // Both add to the same chat while apart: both versions are kept, on both.
  blockSync = true;
  await web2.evaluate(async (id) => {
    const list = chatStore.all();
    list.find((c) => c.id === id).messages.push({ role: 'user', content: [{ type: 'text', text: 'Website side of the fork' }] });
    await chatStore.save(list);
    await ChatSync.syncNow(); await ChatSync.syncNow();
  }, kiwiId);
  await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .item .grow').filter({ hasText: /kiwi/i }).first().click();
  const lime = await ask('Reply with exactly the word: lime');
  ok(/lime/i.test(lime), 'the phone adds to the same chat while it can\'t sync: ' + lime.slice(0, 40));
  blockSync = false;
  await nudge();
  const both = (list) => list.some((c) => JSON.stringify(c.messages).includes('Website side of the fork')) && list.some((c) => JSON.stringify(c.messages).includes('exactly the word: lime'));
  const webFork = await until('both versions on the browser', async () => {
    const list = await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); });
    return both(list) && list;
  });
  ok(webFork.filter((c) => JSON.stringify(c.messages).includes('exactly the word: kiwi')).length === 2, 'two different additions to one chat: both versions kept on the browser');
  await nudge();
  const phoneFork = await until('both versions on the phone', async () => {
    const list = json('chats.json', []).map((m) => json(`chat-${m.id}.json`, {}));
    return list.some((c) => JSON.stringify(c).includes('Website side of the fork')) && list.some((c) => JSON.stringify(c).includes('exactly the word: lime')) && list;
  });
  ok(phoneFork.length === webFork.length, `  and on the phone (${phoneFork.length} chats each)`);

  // Deleted on the website while the phone, offline, adds to it: the chat comes back everywhere,
  // with its image (not a reference to an image the delete took away).
  const PIC_ID = 'conv-' + Date.now() + '-pic';
  await web2.evaluate(async ([id, img]) => {
    await chatStore.save(chatStore.all().concat([{ id, title: 'Picture to keep', model: 'google/gemini-2.5-flash-lite', createdAt: Date.now(), updatedAt: Date.now(),
      messages: [{ role: 'user', content: [{ type: 'text', text: 'Describe this picture' }, { type: 'image_url', image_url: { url: img } }] }, { role: 'assistant', content: 'A test pattern.' }] }]));
    await ChatSync.syncNow(); await ChatSync.syncNow();
  }, [PIC_ID, IMAGE]);
  await nudge();
  await until('the picture chat on the phone', async () => json('chats.json', []).some((m) => m.id === PIC_ID));
  blockSync = true;
  await web2.evaluate(async (id) => { deleteConversation(id); await ChatSync.syncNow(); await ChatSync.syncNow(); }, PIC_ID);
  page = (await launch(false, true)).page;
  await page.locator('.icon', { hasText: '☰' }).click();
  await page.locator('.drawer .item .grow', { hasText: 'Picture to keep' }).click();
  await page.waitForSelector('.msg img');
  // A model that takes images (the chat has one).
  await page.locator('.title.model').click();
  await page.locator('input.search').fill('Gemini 2.5 Flash Lite');
  await page.locator('.pick', { hasText: 'Gemini 2.5 Flash Lite' }).first().locator('.grow').click();
  const grape = await ask('Reply with exactly the word: grape');
  ok(/grape/i.test(grape), 'the phone adds to a chat the website just deleted: ' + grape.slice(0, 40));
  blockSync = false;
  await nudge();
  const back2 = await until('the chat back on the website', async () => {
    const c = (await web2.evaluate(async () => { await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); })).find((x) => x.id === PIC_ID);
    return c && JSON.stringify(c.messages).includes('exactly the word: grape') && c;
  });
  ok(back2.messages[0].content[1].image_url.url === IMAGE, 'deleted on the website while the phone added to it: it comes back, with its image');
  const ctx3 = await browser.newContext();
  const web3 = await ctx3.newPage();
  await web3.goto(WEB + '/');
  await web3.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined');
  await web3.evaluate((s) => { localStorage.setItem('obscurify_private_off', '1'); localStorage.setItem('obscurify_account_kind', 'code'); localStorage.setItem('obscurify_sync_seed', s); }, seed2);
  await web3.reload();
  await web3.waitForFunction(() => window.ChatSync && typeof chatStore !== 'undefined');
  const third = await until('the chat on a new browser', async () => (await web3.evaluate(async () => { await chatStore.ready; await ChatSync.syncNow(); await ChatSync.syncNow(); return getConversations(); })).find((x) => x.id === PIC_ID));
  ok(third.messages[0].content[1].image_url.url === IMAGE, '  a new browser gets it with its image too');

  console.log('\nall sync checks passed');
} finally {
  await browser.close();
  recorder.close();
}
process.exit(0);
