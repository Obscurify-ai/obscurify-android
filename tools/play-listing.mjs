// Pushes play/listing/{title,short_description,full_description}.txt to the Play app's en-US store
// listing, through the release service account. `node tools/play-listing.mjs` shows the diff and
// stops; `--push` sends it (Google reviews listing changes, usually within a day).
import { readFileSync } from 'node:fs';
import { createSign } from 'node:crypto';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const DIR = join(dirname(fileURLToPath(import.meta.url)), '../play/listing');
const SA = process.env.PLAY_SERVICE_ACCOUNT || `${process.env.HOME}/.config/obscurify-android/play-service-account.json`;
const API = 'https://androidpublisher.googleapis.com/androidpublisher/v3/applications/ai.obscurify.play';
const LIMITS = { title: 30, shortDescription: 80, fullDescription: 4000 };

const want = {
  title: readFileSync(join(DIR, 'title.txt'), 'utf8').trim(),
  shortDescription: readFileSync(join(DIR, 'short_description.txt'), 'utf8').trim(),
  fullDescription: readFileSync(join(DIR, 'full_description.txt'), 'utf8').trim(),
};
for (const [k, max] of Object.entries(LIMITS)) if ([...want[k]].length > max) throw new Error(`${k} is ${[...want[k]].length} characters, Play allows ${max}`);

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
  const live = await play('GET', `/edits/${edit.id}/listings/en-US`);
  const changed = Object.keys(LIMITS).filter((k) => live[k] !== want[k]);
  if (!changed.length) { console.log('The Play listing already matches play/listing.'); process.exit(0); }
  for (const k of changed) console.log(`--- ${k} (live)\n${live[k]}\n+++ ${k} (play/listing)\n${want[k]}\n`);
  if (!process.argv.includes('--push')) { console.log('Not sent. Run with --push to send it to Play.'); process.exit(0); }
  await play('PUT', `/edits/${edit.id}/listings/en-US`, { language: 'en-US', ...want });
  await play('POST', `/edits/${edit.id}:validate`);
  await play('POST', `/edits/${edit.id}:commit`);
  committed = true;
  console.log(`Sent ${changed.join(', ')} to Play.`);
} finally {
  if (!committed) await play('DELETE', `/edits/${edit.id}`).catch(() => {});
}
