// OBSCURIFY_E2E_DRIVER. Drives the iPhone app from inside its own page in the iOS Simulator, against
// production: the e2e build (`E2E=1`, the `e2e` cargo feature) runs this script; nothing else ever
// does, and `tools/ios.sh device` refuses a binary with it. It reports through the app's stderr,
// which e2e/ios/host.py reads: "E2E ok - ..." / "E2E FAILED - ..." lines, "E2E-SHOT <name>" for a
// screenshot, "E2E-HOST <what>" for something only the host can do (send the app to the background,
// look at a file's backup flag), "E2E-DONE".
//
// As a guest (no secrets in CI): the AI providers sheet, a chat, Code Mode (Python and a chart in
// the sandbox WebView, a page full screen), Report, and an answer finished in the background. Then,
// with SIGNUP, a new account: the model picker, Account, the session kept out of backups, and the
// account deleted from the app, so nothing is left behind.
const log = s => dioxus.send({log: String(s)});
const sleep = ms => new Promise(r => setTimeout(r, ms));
const $ = s => document.querySelector(s);
const $$ = s => [...document.querySelectorAll(s)];
const text = e => (e && e.textContent || '').trim();
const byText = (sel, t) => $$(sel).find(e => text(e).includes(t));
let failures = 0;
const ok = (c, what) => { if (!c) failures++; log((c ? 'ok - ' : 'FAILED - ') + what); return c; };
async function until(f, ms = 30000, what = '') {
  const t = Date.now();
  while (Date.now() - t < ms) {
    try { const v = f(); if (v) return v; } catch (_) {}
    await sleep(250);
  }
  throw new Error('timed out waiting for ' + what);
}
async function click(sel, t, ms) {
  const el = await until(() => { const e = t ? byText(sel, t) : $(sel); return e && !e.disabled && e; }, ms || 30000, sel + (t ? ' "' + t + '"' : ''));
  el.click();
  await sleep(500);
  return el;
}
async function type(sel, value) {
  const el = await until(() => $(sel), 30000, sel);
  // Not focused: the simulator's keyboard and its first-use tip would cover the screenshots.
  el.value = value;
  el.dispatchEvent(new Event('input', {bubbles: true}));
  await sleep(300);
}
// The host takes the screenshot and says when it has (through the app, e2e-shot-done).
async function shot(name) { await sleep(600); dioxus.send({shot: name}); await dioxus.recv(); }
const host = async (what, ms) => { dioxus.send({host: what}); await sleep(ms); };
const replies = () => $$('.msg.assistant');
const answering = () => !!$('.send.stop');
// Sends a message and waits for the whole answer.
async function ask(t, ms = 240000) {
  const before = replies().length;
  await type('#composer', t);
  await click('.send');
  await until(() => replies().length > before && !answering() && replies().at(-1).querySelector('.meta'), ms, 'the answer to "' + t.slice(0, 40) + '"');
  await sleep(800);
  return replies().at(-1);
}
// Sheets that may come up after signing in, each closed once.
function dismiss() {
  for (const b of $$('.sheet button.bare')) if (/Not now|Later|Cancel/.test(text(b))) { log('closed a sheet: ' + text(b.closest('.sheet').querySelector('h2'))); b.click(); }
}

async function guest() {
  await until(() => $('.login'), 45000, 'the sign-in screen');
  await click('button.guest');
  await until(() => $('#composer'), 15000, 'the chat');
  log('model: ' + text($('.title.model')));

  // App Store 5.1.2(i): nothing goes to a model before the sheet is agreed to.
  const first = 'Give me three short tips for keeping my online life private, as a numbered list with one bold phrase each.';
  await type('#composer', first);
  await click('.send');
  ok(await until(() => $('.sheet.consent'), 5000, 'the AI providers sheet').catch(() => null), 'the AI providers sheet opens before the first message');
  ok($$('.msg.user').length === 0, 'nothing sent before agreeing');
  await shot('consent');
  await click('.sheet.consent button', 'Not now');
  ok(!$('.sheet.consent') && $$('.msg.user').length === 0 && $('#composer').value === first, 'Not now: nothing sent, the message stays in the box');
  await click('.send');
  await click('.sheet.consent button', 'Agree and send');
  await until(() => replies().length && !answering() && replies().at(-1).querySelector('.meta'), 120000, 'the first answer');
  ok($$('.msg.user').length === 1 && $('#composer').value === '', 'Agree and send: sent');
  ok(/\b1\.|<ol/i.test(replies()[0].innerHTML), 'answered: ' + text(replies()[0]).slice(0, 80));
  await sleep(800);
  await shot('chat');

  // Code Mode: the sandbox WKWebView runs Python and draws a chart.
  await click('.plus');
  const code = $('.code-switch');
  ok(!!code, 'the Code switch is in the + menu');
  if (code && !code.classList.contains('on')) code.click();
  const tools = $('.tools-switch');
  if (tools && !tools.classList.contains('on')) tools.click();
  await sleep(300);
  if ($('.menu-veil')) $('.menu-veil').click();
  let r = await ask('Use Python to compute the squares of 1 to 10, plot them as a bar chart, and tell me their sum.');
  ok(!$('.sheet.consent'), 'the AI providers sheet is asked once');
  if (!r.querySelector('.run')) {
    log('no code run; asking again');
    r = await ask('Please call run_code with Python: compute sum(i*i for i in range(1, 11)) and plot the squares as a bar chart with matplotlib.');
  }
  ok(!!r.querySelector('.run.ok'), 'code ran in the sandbox: ' + text(r.querySelector('.run-head')));
  ok(!!r.querySelector('img.run-img'), 'its chart is shown');
  ok(/385/.test(text(r)), 'the sum is 385');
  const chart = r.querySelector('img.run-img');
  if (chart) { chart.scrollIntoView({block: 'center'}); await sleep(800); }
  await shot('code');

  // A page the model builds opens full screen in a sandbox WebView of its own.
  r = await ask('Build a tiny HTML page with a big heading "Hello from the sandbox" and a button that counts clicks. Show it with display_html.');
  const page = r.querySelector('.page-btn');
  ok(!!page, 'the answer has a page to open');
  if (page) {
    page.click();
    await sleep(5000);
    await shot('page');
    dioxus.send({close_page: true});
    await sleep(1000);
  }

  // Report on an answer (the first one, which reads well in a screenshot).
  const report = $$('.msg.assistant .meta button').filter(b => text(b) === 'Report')[0];
  ok(!!report, 'Report on every answer');
  if (report) {
    report.click();
    await until(() => $('.report'), 5000, 'the report form');
    await click('.report .chip', 'Other');
    replies()[0].scrollIntoView({block: 'start'});
    await sleep(500);
    await shot('report');
    await click('.report button', 'Cancel');
  }

}

// An answer that finishes with the app in the background: iOS gives it time, and it says so. Last,
// because the status bar keeps a "back to" link afterwards.
async function background() {
  if ($('.login')) await click('button.guest');
  await until(() => $('#composer'), 15000, 'the chat');
  const before = replies().length;
  await type('#composer', 'Write a story of about 300 words about a lighthouse keeper who finds a message in a bottle.');
  await click('.send');
  await until(() => answering(), 30000, 'the answer to start');
  await host('background', 2000);
  await until(() => replies().length > before && !answering() && replies().at(-1).querySelector('.meta'), 180000, 'the story');
  ok(text(replies().at(-1)).length > 400, 'the story finished: ' + text(replies().at(-1)).length + ' characters');
}

// The account this run made, deleted from Account (also when the run fails after making it).
let madeAccount = false;
async function deleteAccount() {
  if (!$('.big')) {
    dismiss();
    if ($('.drawer')) await click('.drawer .bar .icon');
    await click('.bar .icon');
    await click('.drawer .foot button');
    await until(() => $('.big'), 30000, 'Account');
  }
  await click('button.danger', 'Delete account');
  await click('button.danger', 'Delete for good');
  await until(() => $('.login'), 30000, 'signed out');
  madeAccount = false;
}

async function account() {
  // A guest's model picker asks for an account.
  await click('.title.model');
  await until(() => byText('h1', 'Create Account'), 10000, 'Create Account');
  ok(!$('input[placeholder^="Referral"]'), 'sign-up: no referral code');
  ok(!!byText('label.check', "I'm 18 or older"), 'sign-up: 18+');
  await sleep(3500); // the toast goes
  await shot('signup');
  if (!SIGNUP) return log('no SIGNUP: account screens skipped');
  $('.login .check input').click();
  await click('.login button.go', 'Create Account');
  await until(() => byText('h1', 'Your account code') || $('.login .err'), 30000, 'the account');
  if ($('.login .err')) return log('sign-up refused (skipped): ' + text($('.login .err')));
  madeAccount = true;
  $('.login .check input').click();
  await click('button.go', 'Start Chatting');
  await until(() => $('#composer'), 15000, 'the chat, signed in');
  await sleep(3000);
  dismiss();
  await host('check-backup', 4000);

  // The model picker: no uncensored or roleplay models in the store build.
  await click('.title.model');
  await until(() => $$('.pick').length > 5, 20000, 'the model list');
  ok(!$$('.chip').some(c => /Uncensored|Roleplay/.test(text(c))), 'no Uncensored or Roleplay category');
  // Free models: nothing in the screenshot asks for a plan.
  const free = byText('.chip', 'Free');
  if (free) { free.click(); await sleep(1200); }
  await shot('models');
  await click('.drawer .bar .icon');

  // Account: no way to pay, and Delete account.
  await click('.bar .icon');
  await click('.drawer .foot button');
  await until(() => $('.big'), 30000, 'Account');
  await sleep(1500);
  ok(!byText('button', 'Add funds') && !byText('p', 'Add funds'), 'Account: no Add funds, and no word about buying elsewhere (3.1.3(f))');
  await shot('account');
  await deleteAccount();
  ok(true, 'account deleted from the app');
}

try {
  await until(() => $('#main > *'), 45000, 'the app');
  await guest();
  await account();
  // The sign-in screen (Delete account ends there).
  if (byText('h1', 'Create Account')) await click('a', 'Sign in');
  if (byText('h1', 'Sign In')) { await sleep(4000); await shot('signin'); }
  await background();
} catch (e) {
  ok(false, 'stopped: ' + (e && e.message || e));
  // Never a screenshot of a real account code (artifacts are public).
  if (!byText('h1', 'Your account code')) await shot('error');
  if (madeAccount) {
    if (byText('h1', 'Your account code')) { $('.login .check input').click(); await sleep(500); await click('button.go', 'Start Chatting').catch(() => {}); }
    await deleteAccount().then(() => log('the account made by this run was deleted'), e => ok(false, "couldn't delete the account this run made: " + e.message));
  }
}
dioxus.send({done: failures === 0});
await new Promise(() => {});
