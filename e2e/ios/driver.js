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
// PROXY: the build talks to e2e/flaky-proxy.mjs (host.py runs it), so answers can be scripted.
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
// The host does it (main.rs waits for its e2e-shot-done "host"), then the driver goes on.
const host = async (what, ms = 0) => { dioxus.send({host: what}); await dioxus.recv(); await sleep(ms); };
// The next answers, scripted at the e2e proxy (flaky-proxy.mjs): no model is called.
const script = s => host('script ' + JSON.stringify(s));
const words = n => Array.from({length: n}, (_, i) => 'word' + (i + 1)).join(' ') + ' THE-END';
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
  // The server answers guests with its guest model (DEFAULT_FREE_MODEL); the app names that one.
  ok(/DeepSeek/i.test(text($('.title.model'))), 'the guest model is DeepSeek: ' + text($('.title.model')));

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

// Answers go on with the app in the background (another app in front), every way an answer is made,
// each scripted at the proxy to take a while but less than the half minute iOS gives any app (the
// simulator has no continued processing task, so past that it suspends the app; past the half
// minute is checked on the test iPhone, docs/ios.md): Code Mode with code that runs 15 s, a tool
// the model calls directly, and plain chat. The host sends the app away
// as soon as the answer starts and judges each from the app's own log. Then the app must still
// answer (dioxus#4374: the WebView's edits socket can die in the background). Last, because the
// status bar keeps a "back to" link afterwards.
async function switches(code, tools) {
  await click('.plus');
  for (const [sel, want] of [['.code-switch', code], ['.tools-switch', tools]]) {
    const el = $(sel);
    if (el && el.classList.contains('on') !== want) { el.click(); await sleep(300); }
  }
  if ($('.menu-veil')) $('.menu-veil').click();
  await sleep(300);
}
async function away(name, prompt, most, check) {
  const before = replies().length;
  await type('#composer', prompt);
  await click('.send');
  await until(() => answering(), 30000, name + ': the answer to start');
  await host('background ' + name + ' ' + most);
  await until(() => replies().length > before && !answering() && replies().at(-1).querySelector('.meta'), 120000, name + ': the answer');
  await sleep(800);
  const r = replies().at(-1);
  check(r);
  return r;
}
async function background(signedIn) {
  if ($('.login')) await click('button.guest');
  await until(() => $('#composer'), 15000, 'the chat');
  if (!PROXY) {
    // Production only: a long story, judged as far as it goes (it may end before the app is away).
    await away('story', 'Write a story of about 600 words about a lighthouse keeper who finds a message in a bottle.', 90,
      r => ok(text(r).length > 400, 'the story finished: ' + text(r).length + ' characters'));
    return;
  }
  const who = signedIn ? 'signed-in-' : '';  // no spaces: the host splits on them
  if (!signedIn) {
    await switches(true, true);
    await script([{code: 'import time\nfor i in range(15):\n    print("tick", i, flush=True)\n    time.sleep(1)\nprint("ran 15 s")'},
                  {text: 'The code ran. ' + words(80), slow: 8000}]);
    await away('code', 'Run the slow code.', 120, r => {
      ok(!!r.querySelector('.run.ok') && /ran 15 s/.test(text(r)), 'Code Mode in the background: the code ran to the end (' + text(r.querySelector('.run-head')) + ')');
      ok(/THE-END/.test(text(r)), 'Code Mode in the background: the answer after it finished');
    });
    await script([{tools: [{name: 'web_search', args: {query: 'Obscurify private AI chat'}}]}, {text: 'Searched. ' + words(150), slow: 15000}]);
    await away('tool', 'Search the web, then answer slowly.', 120, r => {
      ok(!!r.querySelector('.tool-call'), 'a direct tool call in the background: its call is shown (' + text(r.querySelector('.tool-call')).slice(0, 60) + ')');
      ok(/THE-END/.test(text(r)), 'a direct tool call in the background: the answer after it finished');
    });
  }
  await switches(false, false);
  await script([{text: words(300), slow: 20000}]);
  await away(who + 'plain', 'Count slowly.', 120, r => ok(/THE-END/.test(text(r)), who + 'plain: plain chat in the background, the whole 20 s answer is there'));
  await script([{text: 'Still here after the background.'}]);
  const r = await ask('Are you still there?', 60000);
  ok(/Still here after the background/.test(text(r)), who + 'after the background: a new message is answered');
}

// Stop, Edit, Retry, the drawer (a chat listed, then deleted), Temp and Wipe: the same Rust and page
// as Android (app.mjs), scripted at the proxy so no model is called.
async function controls() {
  if (!PROXY) return log('no proxy: Stop, Edit, Retry and the drawer skipped');
  if ($('.login')) await click('button.guest');
  await until(() => $('#composer'), 15000, 'the chat');
  await switches(false, false);

  await script([{text: words(200), slow: 20000}]);
  const before = replies().length;
  await type('#composer', 'Count, and I will stop you.');
  await click('.send');
  await until(() => answering() && replies().length > before && /word3\b/.test(text(replies().at(-1))), 30000, 'the answer to stream');
  await click('.send.stop');
  await until(() => !answering(), 15000, 'Stop to end the answer');
  await sleep(1500);
  const stopped = text(replies().at(-1));
  await sleep(2500);
  ok(!/THE-END/.test(stopped) && text(replies().at(-1)) === stopped, 'Stop: the answer ends where it was and nothing more comes (' + stopped.length + ' characters)');

  const edit = $$('.msg.assistant .meta button').filter(b => text(b) === 'Edit').at(-1);
  ok(!!edit, 'Edit on the answer');
  if (edit) {
    edit.click();
    await sleep(800);
    ok($('#composer').value.includes('I will stop you') && !$$('.msg.user').some(m => text(m).includes('I will stop you')),
      'Edit: the question is back in the box, it and its answer gone from the chat');
  }

  await script([{error: 'The provider had a problem (scripted).'}, {text: 'Answered on the retry.'}]);
  await type('#composer', 'This one fails first.');
  await click('.send');
  const retry = await until(() => !answering() && byText('.msg.assistant .meta button', 'Retry'), 60000, 'Retry on a failed answer').catch(() => null);
  ok(!!retry, 'a failed answer shows Retry');
  if (retry) {
    retry.click();
    await until(() => !answering() && /Answered on the retry/.test(text(replies().at(-1))), 60000, 'the retried answer').catch(() => {});
    ok(/Answered on the retry/.test(text(replies().at(-1))), 'Retry: answered');
  }

  await click('.bar .icon');
  await until(() => $('.drawer'), 5000, 'the drawer');
  const item = await until(() => byText('.drawer .item', 'This one fails first') || $('.drawer .item'), 10000, 'the chat in the drawer').catch(() => null);
  ok(!!item, 'the chat is listed in the drawer');
  const n = $$('.drawer .item').length;
  if (item) {
    item.querySelector('button.bare').click();
    await sleep(400);
    await click('.drawer .item button.danger', 'Delete');
    ok($$('.drawer .item').length === n - 1, 'the chat is deleted from the drawer');
  }
  await click('#temp');
  await sleep(800);
  ok($('#composer').placeholder === 'Temporary chat', 'Temp: a temporary chat');
  await script([{text: 'A temporary answer.'}]);
  await ask('Something to forget.', 60000);
  await click('.bar .icon');
  await until(() => $('.drawer'), 5000, 'the drawer');
  ok(!byText('.drawer .item', 'Something to forget'), 'and it is not kept in the drawer');
  await click('#wipe');
  await click('#wipe-yes');
  await sleep(1500);
  if (!$('.drawer')) await click('.bar .icon');
  ok(await until(() => $$('.drawer .item').length === 0, 10000, 'no chats').catch(() => false), 'Wipe: no chats left');
  if ($('.drawer')) await click('.drawer .bar .icon').catch(() => {});
}

// The account this run made, deleted from Account (also when the run fails after making it).
let madeAccount = false;
let accountCode = '';
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
  accountCode = text($('.number'));
  $('.login .check input').click();
  await click('button.go', 'Start Chatting');
  await until(() => $('#composer'), 15000, 'the chat, signed in');
  await sleep(3000);
  dismiss();
  await host('check-backup');
  // Signed in, the server holds the answer too (`background: true`): it goes on in the app as well.
  if (PROXY) await background(true);

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

  // Sign out, then back in with the account code typed loosely (lower case, spaces for dashes).
  // The code stays in this page: never logged, never on screen in a screenshot.
  if (accountCode) {
    await click('.section button.danger', 'Sign out');
    await click('.section .row button.danger', 'Sign out');
    await until(() => $('.login'), 30000, 'signed out');
    if (byText('h1', 'Create Account')) await click('a', 'Sign in');
    await until(() => $('#login-words'), 10000, 'the sign-in field');
    await type('#login-words', accountCode.toLowerCase().replace(/-/g, ' '));
    await click('.login button.go', 'Sign In');
    ok(await until(() => $('#composer'), 60000, 'the chat').catch(() => null), 'signed out, then back in with the account code');
    await sleep(2000);
    dismiss();
  }
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
  await controls();
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
