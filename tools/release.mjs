#!/usr/bin/env node
// Releases the app when main gets a new version, run by a systemd timer on the release machine (the
// signing keys never leave it). For the `version` in Cargo.toml on origin/main:
//   1. GitHub: `./build.sh` (arm64, release key) -> checks -> release v<version> with obscurify.apk as
//      its only file and fastlane/metadata/android/en-US/changelogs/<version-code>.txt as the notes.
//   2. Play: `./build.sh play` (upload key) -> checks -> Google Play production track, with
//      play/changelogs/<version-code>.txt as the notes if there is one (the Play app has no payments,
//      so a release about them needs other notes there), else the same notes.
// Each step is done once per version (state.json), checked against GitHub/Play before building, so
// a retry never redoes a step that went through. A failure is retried on the next run and Signals
// Tony once per version, step and error.
//
// Builds run in a clone of its own (never a working checkout): ~/.local/share/obscurify-release/repo.
// Play waits while ~/.config/obscurify-android/play-hold exists (its text says why). Play needs a
// service account key with release rights on this app: ~/.config/obscurify-android/play-service-account.json.
//   node tools/release.mjs            one run (what the timer does)
//   node tools/release.mjs --dry-run  say what it would do, build and publish nothing
//   node tools/release.mjs --check-apk target/obscurify.apk 1000835 | --check-aab target/obscurify-play.aab
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { createSign } from 'node:crypto';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

const HOME = homedir();
const STATE_DIR = join(HOME, '.local/share/obscurify-release');
const REPO = join(STATE_DIR, 'repo');
const STATE = join(STATE_DIR, 'state.json');
const KEYS = join(HOME, '.config/obscurify-android');
const HOLD = join(KEYS, 'play-hold');
const SERVICE_ACCOUNT = join(KEYS, 'play-service-account.json');
const ORIGIN = 'https://github.com/Obscurify-ai/obscurify-android.git';
const GH_REPO = 'Obscurify-ai/obscurify-android';
const PLAY_PACKAGE = 'ai.obscurify.play';
const RELEASE_CERT = '7ca401a8fed219c17f197af6b07c10d063097fb6399890b58b58d58cd872a71c';
const UPLOAD_CERT = '1187182f16c62eefa59fa59501c3b1fc9075a0a64c7ef2b60b2bdd27e53ff065';
const GUARD = 'Never write sexually explicit content';
const DRY = process.argv.includes('--dry-run');

const log = (...a) => console.log(new Date().toISOString(), ...a);
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { encoding: 'utf8', maxBuffer: 1 << 28, ...opts });
const git = (...a) => run('git', ['-C', REPO, ...a]).trim();
const load = () => (existsSync(STATE) ? JSON.parse(readFileSync(STATE, 'utf8')) : {});
const save = (s) => writeFileSync(STATE, JSON.stringify(s, null, 2));

function notify(state, key, text) {
  state.notified ??= {};
  if (state.notified[key]) return;
  state.notified[key] = new Date().toISOString();
  save(state);
  if (DRY) return log('would Signal:', text);
  try { run('signal-me', [text]); } catch (e) { log('signal-me failed', e.message); }
}

// ---- the commit to release ---------------------------------------------------------------------

function checkout() {
  mkdirSync(STATE_DIR, { recursive: true });
  if (!existsSync(join(REPO, '.git'))) run('git', ['clone', '--quiet', ORIGIN, REPO]);
  git('fetch', '--quiet', 'origin');
  git('checkout', '--quiet', '--force', '--detach', 'origin/main');
  // A clean tree at exactly origin/main; Rust and Gradle caches under target/ are kept.
  git('clean', '-ffdxq', '-e', 'target', '-e', 'e2e/node_modules');
  const cargo = readFileSync(join(REPO, 'Cargo.toml'), 'utf8');
  const version = cargo.match(/^version = "(.+)"/m)[1];
  const code = cargo.match(/^version-code = (\d+)/m)[1];
  const notesFile = join(REPO, `fastlane/metadata/android/en-US/changelogs/${code}.txt`);
  if (!existsSync(notesFile)) throw new Error(`no changelog for ${version}: fastlane/metadata/android/en-US/changelogs/${code}.txt`);
  const notes = readFileSync(notesFile, 'utf8').trim();
  const playNotesFile = join(REPO, `play/changelogs/${code}.txt`);
  const playNotes = existsSync(playNotesFile) ? readFileSync(playNotesFile, 'utf8').trim() : notes;
  return { version, code, sha: git('rev-parse', 'HEAD'), notes, playNotes, notesFile };
}

function build(args) {
  // Never a test build: no other server, no WebView debugging.
  const env = { ...process.env };
  delete env.OBSCURIFY_API;
  delete env.DEVTOOLS;
  log('building', args.join(' ') || '(sideload)');
  run('./build.sh', args, { cwd: REPO, env, stdio: ['ignore', 'pipe', 'pipe'] });
}

// ---- checks on what was built ------------------------------------------------------------------

const buildTools = () => join(HOME, 'Android/Sdk/build-tools', readdirSync(join(HOME, 'Android/Sdk/build-tools')).sort().pop());
const javaEnv = () => {
  let java = '';
  try { java = run('mise', ['where', 'java@temurin-21']).trim(); } catch {}
  return java ? { ...process.env, JAVA_HOME: java, PATH: `${java}/bin:${process.env.PATH}` } : process.env;
};
function unzipTo(file) {
  const dir = mkdtempSync(join(tmpdir(), 'obs-release-'));
  run('unzip', ['-qq', '-o', file, '-d', dir]);
  return dir;
}
const contains = (file, text) => readFileSync(file).includes(Buffer.from(text));

function checkApk(apk, code) {
  const bt = buildTools();
  const badging = run(join(bt, 'aapt2'), ['dump', 'badging', apk]);
  const pkg = badging.match(/package: name='([^']+)' versionCode='(\d+)'/);
  if (!pkg || pkg[1] !== 'ai.obscurify.android' || pkg[2] !== code) throw new Error(`APK is ${pkg?.[1]} ${pkg?.[2]}, expected ai.obscurify.android ${code}`);
  const certs = run(join(bt, 'apksigner'), ['verify', '--print-certs', apk], { env: javaEnv() });
  if (!certs.replace(/:/g, '').toLowerCase().includes(RELEASE_CERT)) throw new Error('APK is not signed with the release key');
  const dir = unzipTo(apk);
  try {
    const abis = readdirSync(join(dir, 'lib'));
    if (abis.join() !== 'arm64-v8a') throw new Error('APK libraries: ' + abis.join(', '));
    const so = join(dir, 'lib/arm64-v8a/libmain.so');
    if (!contains(so, 'https://obscurify.ai') || contains(so, '127.0.0.1:8093')) throw new Error('APK is not pointed at obscurify.ai');
    if (contains(so, GUARD)) throw new Error('APK is a Play build');
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

function checkAab(aab) {
  const certs = run('jarsigner', ['-verify', '-certs', '-verbose', aab], { env: javaEnv() });
  if (!/jar verified/.test(certs)) throw new Error('bundle signature does not verify');
  // jarsigner prints the signer's certificate; compare its SHA-256 with keytool.
  const printed = run('keytool', ['-printcert', '-jarfile', aab], { env: javaEnv() });
  if (!printed.replace(/:/g, '').toLowerCase().includes(UPLOAD_CERT)) throw new Error('bundle is not signed with the Play upload key');
  const dir = unzipTo(aab);
  try {
    const abis = readdirSync(join(dir, 'base/lib')).sort();
    if (abis.join() !== 'arm64-v8a,x86_64') throw new Error('bundle libraries: ' + abis.join(', '));
    for (const abi of abis) {
      const so = join(dir, 'base/lib', abi, 'libmain.so');
      if (!contains(so, GUARD)) throw new Error(`bundle ${abi} library has no content guard: not a Play build`);
      if (!contains(so, 'https://obscurify.ai') || contains(so, '127.0.0.1:8093')) throw new Error('bundle is not pointed at obscurify.ai');
    }
    if (!contains(join(dir, 'base/manifest/AndroidManifest.xml'), PLAY_PACKAGE)) throw new Error('bundle is not ' + PLAY_PACKAGE);
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

// ---- GitHub ------------------------------------------------------------------------------------

function githubHas(version) {
  try { run('gh', ['release', 'view', `v${version}`, '-R', GH_REPO, '--json', 'tagName']); return true; } catch { return false; }
}

function releaseGithub(r) {
  if (DRY) return log(`would build and publish GitHub release v${r.version}`);
  build([]);
  const apk = join(REPO, 'target/obscurify.apk');
  checkApk(apk, r.code);
  run('gh', ['release', 'create', `v${r.version}`, apk, '-R', GH_REPO, '--target', r.sha, '--title', r.version, '--notes-file', r.notesFile]);
  log(`published GitHub release v${r.version}`);
}

// ---- Google Play (Android Publisher API v3) ----------------------------------------------------

async function playToken() {
  const sa = JSON.parse(readFileSync(SERVICE_ACCOUNT, 'utf8'));
  const now = Math.floor(Date.now() / 1000);
  const b64 = (o) => Buffer.from(JSON.stringify(o)).toString('base64url');
  const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64({ iss: sa.client_email, scope: 'https://www.googleapis.com/auth/androidpublisher', aud: sa.token_uri, iat: now, exp: now + 3600 })}`;
  const sig = createSign('RSA-SHA256').update(unsigned).sign(sa.private_key, 'base64url');
  const r = await fetch(sa.token_uri, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'urn:ietf:params:oauth:grant-type:jwt-bearer', assertion: `${unsigned}.${sig}` }) });
  const j = await r.json();
  if (!j.access_token) throw new Error('Play sign-in failed: ' + JSON.stringify(j));
  return j.access_token;
}

async function play(token, method, path, body, upload) {
  const base = upload ? 'https://androidpublisher.googleapis.com/upload/androidpublisher/v3' : 'https://androidpublisher.googleapis.com/androidpublisher/v3';
  // A dropped connection is tried again (three times, a little later each time), with its cause logged.
  let r;
  for (let attempt = 1; ; attempt++) {
    try {
      r = await fetch(`${base}/applications/${PLAY_PACKAGE}${path}`, {
        method, headers: { authorization: `Bearer ${token}`, ...(upload ? { 'content-type': 'application/octet-stream' } : body ? { 'content-type': 'application/json' } : {}) },
        body: upload ?? (body ? JSON.stringify(body) : undefined),
        signal: AbortSignal.timeout(upload ? 600_000 : 60_000),
      });
      break;
    } catch (e) {
      const why = `${e.message}${e.cause ? ': ' + (e.cause.code || e.cause.message) : ''}`;
      log(`Play ${method} ${path.split('?')[0]}: ${why} (try ${attempt})`);
      if (attempt >= 3) throw new Error(`Play ${method} ${path.split('?')[0]}: ${why}`);
      await new Promise((ok) => setTimeout(ok, attempt * 15_000));
    }
  }
  const text = await r.text();
  if (!r.ok) throw new Error(`Play ${method} ${path.split('?')[0]}: ${r.status} ${text.slice(0, 500)}`);
  return text ? JSON.parse(text) : {};
}

/** Whether any Play track already has this versionCode (then it was uploaded before). */
async function playHas(token, code) {
  const edit = await play(token, 'POST', '/edits', {});
  try {
    const { tracks = [] } = await play(token, 'GET', `/edits/${edit.id}/tracks`);
    return tracks.some((t) => (t.releases || []).some((rel) => (rel.versionCodes || []).includes(String(code))));
  } finally {
    await play(token, 'DELETE', `/edits/${edit.id}`).catch(() => {});
  }
}

async function releasePlay(r) {
  const token = await playToken();
  if (await playHas(token, r.code)) return log(`Play already has ${r.code}`);
  if (DRY) return log(`would build and upload ${r.version} (${r.code}) to Play production`);
  build(['play']);
  const aab = join(REPO, 'target/obscurify-play.aab');
  checkAab(aab);
  const edit = await play(token, 'POST', '/edits', {});
  const up = await play(token, 'POST', `/edits/${edit.id}/bundles?uploadType=media`, null, readFileSync(aab));
  if (String(up.versionCode) !== r.code) throw new Error(`Play read versionCode ${up.versionCode}, expected ${r.code}`);
  await play(token, 'PUT', `/edits/${edit.id}/tracks/production`, {
    track: 'production',
    releases: [{ name: r.version, versionCodes: [r.code], status: 'completed', releaseNotes: [{ language: 'en-US', text: r.playNotes.slice(0, 500) }] }],
  });
  await play(token, 'POST', `/edits/${edit.id}:commit`);
  log(`sent ${r.version} (${r.code}) to Play production review`);
}

// ---- one run -----------------------------------------------------------------------------------

// `--check-apk FILE CODE` / `--check-aab FILE`: only run the checks on a built file.
const at = (flag) => process.argv.indexOf(flag);
if (at('--check-apk') > 0) { checkApk(process.argv[at('--check-apk') + 1], process.argv[at('--check-apk') + 2]); log('APK checks passed'); process.exit(0); }
if (at('--check-aab') > 0) { checkAab(process.argv[at('--check-aab') + 1]); log('bundle checks passed'); process.exit(0); }

const state = load();
let r;
try {
  r = checkout();
} catch (e) {
  log('checkout failed:', e.message);
  notify(state, 'checkout:' + e.message.slice(0, 80), `Android release: couldn't read main: ${e.message.slice(0, 300)}`);
  process.exit(1);
}
state[r.version] ??= {};
const done = state[r.version];
let failed = false;

if (!done.github) {
  try {
    if (githubHas(r.version)) log(`GitHub already has v${r.version}`);
    else releaseGithub(r);
    if (!DRY) { done.github = new Date().toISOString(); save(state); }
  } catch (e) {
    failed = true;
    log('GitHub release failed:', e.stderr || e.message);
    notify(state, `${r.version}:github:${e.message.slice(0, 80)}`, `Android ${r.version}: the GitHub release failed, retrying every few minutes. ${String(e.stderr || e.message).slice(-400)}`);
  }
}

if (!done.play) {
  if (existsSync(HOLD)) {
    log(`Play held: ${readFileSync(HOLD, 'utf8').trim()}`);
  } else if (!existsSync(SERVICE_ACCOUNT)) {
    log('Play skipped: no service account key at', SERVICE_ACCOUNT);
    notify(state, 'play:no-key', 'Android releases: Play uploads need ~/.config/obscurify-android/play-service-account.json.');
  } else {
    try {
      await releasePlay(r);
      if (!DRY) { done.play = new Date().toISOString(); save(state); }
    } catch (e) {
      failed = true;
      log('Play release failed:', e.stderr || e.message);
      notify(state, `${r.version}:play:${e.message.slice(0, 80)}`, `Android ${r.version}: the Play upload failed, retrying every few minutes. ${String(e.stderr || e.message).slice(-400)}`);
    }
  }
}
if (!DRY) save(state);
process.exit(failed ? 1 : 0);
