// In-app Tor: off by default (nothing started, chat goes to obscurify.ai), the Tor button connects
// through Tor and chat goes to the onion address, "Always connect through Tor" starts Tor with the app
// and nothing reaches obscurify.ai directly, Code is off through Tor, and Tor off goes back to normal.
// Every few hundred ms the app's open TCP connections are read (/proc/<pid>/net/tcp*): while Tor is
// on, none may go to obscurify.ai's address.
//   node e2e/tor.mjs      (test APK built with DEVTOOLS=1, against the live site; a rooted emulator)
import { launch, ok, sleep, adb, PKG } from './lib.mjs';
import { resolve4 } from 'node:dns/promises';

const site = new Set(await resolve4('obscurify.ai'));
const files = () => adb('shell', 'ls', `/data/user/0/${PKG}/files`).split(/\s+/).filter(Boolean);

// IPv4 remote addresses of the app's TCP sockets (and IPv4-mapped IPv6 ones).
function remotes() {
  let pid;
  try { pid = adb('shell', 'pidof', PKG); } catch { return [] }
  const out = adb('shell', 'cat', `/proc/${pid}/net/tcp`, `/proc/${pid}/net/tcp6`);
  const uid = adb('shell', 'stat', '-c', '%u', `/proc/${pid}`);
  const ips = [];
  for (const line of out.split('\n')) {
    const f = line.trim().split(/\s+/);
    if (f.length < 8 || f[7] !== uid) continue;
    const hex = f[2].split(':')[0];
    const v4 = hex.length === 8 ? hex : hex.length === 32 && hex.startsWith('0000000000000000FFFF0000') ? hex.slice(24) : null;
    if (!v4) continue;
    ips.push({ ip: [6, 4, 2, 0].map(i => parseInt(v4.slice(i, i + 2), 16)).join('.'), inode: f[9] });
  }
  return ips;
}

// Watches connections until stopped; returns every address seen on a socket that wasn't already
// open when the watch began (an idle keep-alive from before isn't new traffic).
function watch(all) {
  const before = new Set(all ? [] : remotes().map(r => r.inode));
  const seen = new Set();
  let on = true;
  (async () => { while (on) { for (const r of remotes()) if ((all || r.inode !== '0') && !before.has(r.inode)) seen.add(r.ip); await sleep(300); } })();
  return { stop: () => { on = false; return [...seen]; } };
}

const say = async (page, text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 15000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 240000 });
  await sleep(500);
  return page.locator('.msg').last().textContent();
};
const drawer = async (page) => { await page.locator('.icon', { hasText: '☰' }).click(); await page.waitForSelector('.drawer .quick'); };
const closeDrawer = async (page) => { await page.locator('.veil').click({ position: { x: 380, y: 300 } }).catch(() => {}); await sleep(300); };
const torOn = async (page) => page.waitForFunction(() => document.querySelector('#tor')?.classList.contains('on'), null, { timeout: 240000, polling: 500 });

// 1. Tor off: an ordinary guest chat, straight to obscurify.ai, and Tor never started.
let { page } = await launch(true);
const guest = page.getByRole('button', { name: /guest/i });
if (await guest.count()) await guest.first().click();
await page.waitForSelector('#composer', { timeout: 30000 });
let w = watch(true);
ok(/pelican/i.test(await say(page, 'Reply with the single word PELICAN.')), 'Tor off: a guest chat is answered');
let seen = w.stop();
ok(seen.some(ip => site.has(ip)), `Tor off: the app talked to obscurify.ai directly (${seen.join(' ')})`);
ok(!files().includes('tor'), 'Tor off: Tor never started (no tor/ dir)');
await drawer(page);
ok(await page.locator('#tor-always').count() === 0, 'Tor off: no "Always" row (the drawer looks as before)');

// 2. The Tor button connects, and chat goes over Tor.
const t0 = Date.now();
w = watch();
await page.locator('#tor').click();
ok(await page.locator('#tor.busy').count() === 1 || await page.locator('#tor.on').count() === 1, 'Tor shows connecting');
await torOn(page);
console.log(`   Tor connected in ${((Date.now() - t0) / 1000).toFixed(1)} s`);
ok(await page.locator('#tor-always').count() === 1, 'the "Always connect through Tor" row appears');
await closeDrawer(page);
await page.locator('.icon', { hasText: '＋' }).click().catch(() => {});
const t1 = Date.now();
ok(/otter/i.test(await say(page, 'Reply with the single word OTTER.')), 'Tor on: a guest chat is answered over Tor');
console.log(`   answered in ${((Date.now() - t1) / 1000).toFixed(1)} s`);
seen = w.stop();
ok(!seen.some(ip => site.has(ip)), `Tor on: no connection to obscurify.ai's address (${seen.join(' ')})`);
ok(files().includes('tor'), 'Tor keeps its state in the app files dir');

// Code is off through Tor (the + menu's Code switch says why).
await page.locator('.plus').click();
const code = page.locator('.code-switch');
if (await code.count()) ok(await code.getAttribute('aria-disabled') === 'true', 'Tor on: the Code switch is off');
await page.locator('.menu-veil').click({ position: { x: 10, y: 10 } }).catch(() => {});

// 3. Always connect through Tor: on after a restart, from the start, nothing direct.
await drawer(page);
await page.locator('#tor-always').check();
ok(files().includes('tor.json'), '"Always" is saved');
adb('shell', 'am', 'force-stop', PKG);
w = watch();
({ page } = await launch(false, true));
// A guest starts at sign-in each time.
if (await page.getByRole('button', { name: /guest/i }).count()) await page.getByRole('button', { name: /guest/i }).first().click();
await drawer(page);
ok(await page.locator('#tor.busy').count() + await page.locator('#tor.on').count() === 1, 'Always: Tor is starting with the app');
await torOn(page);
await closeDrawer(page);
ok(/heron/i.test(await say(page, 'Reply with the single word HERON.')), 'Always: a chat is answered over Tor');
seen = w.stop();
ok(!seen.some(ip => site.has(ip)), `Always: from launch on, nothing went to obscurify.ai's address (${seen.join(' ')})`);

// Tor can't be turned off while "Always" is on.
await drawer(page);
await page.locator('#tor').click();
await sleep(500);
ok(await page.locator('#tor.on').count() === 1, 'Always: tapping Tor keeps Tor on');

// 4. Always off, Tor off: back to normal.
await page.locator('#tor-always').uncheck();
ok(!files().includes('tor.json'), '"Always" off is saved');
await page.locator('#tor').click();
await sleep(500);
ok(await page.locator('#tor.on').count() === 0 && await page.locator('#tor.busy').count() === 0, 'Tor off');
await closeDrawer(page);
w = watch(true);
ok(/badger/i.test(await say(page, 'Reply with the single word BADGER.')), 'Tor off again: a chat is answered');
seen = w.stop();
ok(seen.some(ip => site.has(ip)), 'Tor off again: straight to obscurify.ai');
console.log('tor: all passed');
process.exit(0);
