// Pushes the Play app's store listing text to Google Play, through the release service account.
// play/listing/{title,short_description,full_description}.txt is the en-US (default) listing;
// each play/listing/<locale>/ folder with the same three files is a translated listing (Play
// language codes: es-419, pt-BR, ja-JP, ...). Translated listings without their own images use the
// default listing's.
// `node tools/play-listing.mjs` shows what differs from the live listing and stops; `--push` sends
// every changed language in one edit (Google reviews listing changes). `--only es-419,de-DE` limits it.
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { createSign } from 'node:crypto';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const DIR = join(dirname(fileURLToPath(import.meta.url)), '../play/listing');
const SA = process.env.PLAY_SERVICE_ACCOUNT || `${process.env.HOME}/.config/obscurify-android/play-service-account.json`;
const API = 'https://androidpublisher.googleapis.com/androidpublisher/v3/applications/ai.obscurify.play';
const LIMITS = { title: 30, shortDescription: 80, fullDescription: 4000 };
const at = (flag) => process.argv.indexOf(flag);
const only = at('--only') > 0 ? process.argv[at('--only') + 1].split(',') : null;

function read(dir) {
  return {
    title: readFileSync(join(dir, 'title.txt'), 'utf8').trim(),
    shortDescription: readFileSync(join(dir, 'short_description.txt'), 'utf8').trim(),
    fullDescription: readFileSync(join(dir, 'full_description.txt'), 'utf8').trim(),
  };
}
const wanted = { 'en-US': read(DIR) };
for (const name of readdirSync(DIR).sort()) {
  const d = join(DIR, name);
  if (statSync(d).isDirectory() && existsSync(join(d, 'title.txt'))) wanted[name] = read(d);
}
for (const [lang, w] of Object.entries(wanted))
  for (const [k, max] of Object.entries(LIMITS))
    if ([...w[k]].length > max) throw new Error(`${lang} ${k} is ${[...w[k]].length} characters, Play allows ${max}`);

const sa = JSON.parse(readFileSync(SA, 'utf8'));
const now = Math.floor(Date.now() / 1000);
const b64 = (o) => Buffer.from(JSON.stringify(o)).toString('base64url');
const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64({ iss: sa.client_email, scope: 'https://www.googleapis.com/auth/androidpublisher', aud: sa.token_uri, iat: now, exp: now + 3600 })}`;
const assertion = `${unsigned}.${createSign('RSA-SHA256').update(unsigned).sign(sa.private_key, 'base64url')}`;
const { access_token } = await (await fetch(sa.token_uri, { method: 'POST', body: new URLSearchParams({ grant_type: 'urn:ietf:params:oauth:grant-type:jwt-bearer', assertion }) })).json();

async function play(method, path, body) {
  const r = await fetch(API + path, { method, headers: { authorization: `Bearer ${access_token}`, ...(body ? { 'content-type': 'application/json' } : {}) }, body: body && JSON.stringify(body) });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${text.slice(0, 500)}`);
  return text ? JSON.parse(text) : {};
}

const edit = await play('POST', '/edits', {});
let committed = false;
try {
  const { listings = [] } = await play('GET', `/edits/${edit.id}/listings`);
  const live = Object.fromEntries(listings.map((l) => [l.language, l]));
  const changes = [];
  for (const [lang, w] of Object.entries(wanted)) {
    if (only && !only.includes(lang)) continue;
    const fields = Object.keys(LIMITS).filter((k) => live[lang]?.[k] !== w[k]);
    if (!fields.length) continue;
    changes.push(lang);
    if (!live[lang]) { console.log(`+ ${lang} (new language): ${w.title} / ${w.shortDescription}`); continue; }
    for (const k of fields) console.log(`--- ${lang} ${k} (live)\n${live[lang][k]}\n+++ ${lang} ${k} (play/listing)\n${w[k]}\n`);
  }
  const extra = Object.keys(live).filter((l) => !wanted[l]);
  if (extra.length) console.log(`Live languages with no folder here (left alone): ${extra.join(', ')}`);
  if (!changes.length) { console.log('The Play listing already matches play/listing.'); process.exit(0); }
  if (!process.argv.includes('--push')) { console.log(`${changes.length} language(s) differ. Not sent; run with --push.`); process.exit(0); }
  for (const lang of changes) await play('PUT', `/edits/${edit.id}/listings/${lang}`, { language: lang, ...wanted[lang] });
  await play('POST', `/edits/${edit.id}:validate`);
  await play('POST', `/edits/${edit.id}:commit`);
  committed = true;
  console.log(`Sent ${changes.join(', ')} to Play.`);
} finally {
  if (!committed) await play('DELETE', `/edits/${edit.id}`).catch(() => {});
}
