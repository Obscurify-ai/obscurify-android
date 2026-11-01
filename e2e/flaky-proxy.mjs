// Sits between the app and a server and breaks chat completions on demand, for resume.mjs. Build the
// test APK with OBSCURIFY_API=http://127.0.0.1:8093, `adb reverse tcp:8093 tcp:8093`, then run this.
//   GET /__set?after=1&fail=3&kind=error|cut|hang   pass `after` completions, then break `fail` of them
//   GET /__set?tooldown=20000 | chatdown=20000 | losecall=1   an outage, or a paid call's answer lost
//   GET /__log, /__tools                             the completion and tool request bodies so far
import http from 'node:http';
import https from 'node:https';

const TARGET = new URL(process.env.TARGET || 'https://obscurify.ai');
const self = 'http://127.0.0.1:8093';
let rule = { after: 0, fail: 0, kind: 'error' };
let log = [];
let tools = [];
// `outage` ms of nothing working, starting at the next tool call (tooldown) or completion (chatdown).
let down = { tool: 0, chat: 0, until: 0 };
// The next paid call really runs, but its answer never reaches the app.
let losecall = 0;

function forward(req, res, body, onChunk, onEnd) {
  const headers = { ...req.headers, host: TARGET.host, 'accept-encoding': 'identity' };
  if (headers.origin) headers.origin = TARGET.origin;
  const up = https.request({ host: TARGET.hostname, path: req.url, method: req.method, headers }, (r) => {
    const h = { ...r.headers };
    // Pages are served from here in the test, so their CSP has to allow here.
    for (const k of Object.keys(h)) if (k.toLowerCase() === 'content-security-policy') h[k] = String(h[k]).replaceAll(TARGET.origin, self);
    if (!onEnd) res.writeHead(r.statusCode, h);
    r.on('data', (c) => { if (!onChunk || onChunk(c)) res.write(c); });
    r.on('end', () => (onEnd ? onEnd() : res.end()));
  });
  up.on('error', () => res.destroy());
  up.end(body);
}

http.createServer((req, res) => {
  const url = new URL(req.url, self);
  if (url.pathname === '/__set') {
    rule = { after: +url.searchParams.get('after') || 0, fail: +url.searchParams.get('fail') || 0, kind: url.searchParams.get('kind') || 'error' };
    down = { tool: +url.searchParams.get('tooldown') || 0, chat: +url.searchParams.get('chatdown') || 0, until: 0 };
    losecall = +url.searchParams.get('losecall') || 0;
    log = [];
    tools = [];
    return res.end('ok');
  }
  if (url.pathname === '/__log') return res.end(JSON.stringify(log));
  if (url.pathname === '/__tools') return res.end(JSON.stringify(tools));
  if (down.tool && /\/tools\//.test(url.pathname)) { down.until = Date.now() + down.tool; down.tool = 0; }
  if (down.chat && /chat\/completions$/.test(url.pathname) && log.length >= 1) { down.until = Date.now() + down.chat; down.chat = 0; }
  if (Date.now() < down.until) return req.socket.destroy();
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    if (/\/tools\//.test(url.pathname)) {
      tools.push({ path: url.pathname, body: JSON.parse(body.toString() || 'null') });
      if (losecall > 0 && url.pathname.endsWith('/call')) {
        losecall--;
        return forward(req, res, body, () => false, () => req.socket.destroy());
      }
    }
    if (!/chat\/completions$/.test(url.pathname)) return forward(req, res, body);
    log.push(JSON.parse(body.toString()));
    if (rule.after > 0) { rule.after--; return forward(req, res, body); }
    if (rule.fail <= 0) return forward(req, res, body);
    rule.fail--;
    if (rule.kind === 'hang') return;
    if (rule.kind === 'error') {
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      return res.end('data: {"error":{"message":"partial tool call received"}}\n\n');
    }
    // Up to a few lines of text, then the connection goes: never a whole answer.
    let lines = 0;
    forward(req, res, body, (c) => {
      for (const line of c.toString().split(/(?<=\n)/)) {
        if (res.destroyed) return false;
        if (line.includes('[DONE]')) { res.destroy(); return false; }
        res.write(line);
        if (/"content":"[^"]/.test(line) && ++lines >= 3) { setTimeout(() => res.destroy(), 50); return false; }
      }
      return false;
    });
  });
}).listen(8093, '127.0.0.1', () => console.log('flaky proxy on 8093'));
