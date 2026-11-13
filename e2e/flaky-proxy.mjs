// Sits between the app and a server and breaks chat completions on demand, for resume.mjs. Build the
// test APK with OBSCURIFY_API=http://127.0.0.1:8093, `adb reverse tcp:8093 tcp:8093`, then run this.
//   GET /__set?after=1&fail=3&kind=error|cut|hang|freeze   pass `after` completions, then break `fail` of them
//     (freeze: a few lines, then the connection stays open and nothing more comes, as after a phone slept)
//   GET /__set?tooldown=20000 | chatdown=20000 | losecall=1   an outage, or a paid call's answer lost
//   GET /__set?toolbusy=2                            paid calls answer 429 once, then succeed without running (-1: 429 always)
//   GET /__set?silent=N                              the next N streamed completions send headers, then nothing, open
//   GET /__set?toolfail=N                            tool requests answer 503 with an error N times (-1: always)
//   GET /__set?sandboxdown=1                         the Code Mode sandbox page answers 500
//   GET /__set?context=N                             /models lists every model with an N-token context
//   GET /__log, /__tools                             the completion and tool request bodies so far
//   POST /__script  [{...}, ...]                     canned replies for the next completions, in order:
//     {code, usage: {prompt_tokens, completion_tokens, cost}} a run_code call; {text, usage} an answer;
//     {error: "..."} a failure; {status: 502} a proxy's error page. No model is called, so these cost nothing.
//     Any of them may carry {reasoning: "..."} (streamed first); {tools: [{name, args}]} makes direct
//     tool calls instead, after `text` if there is one.
//   POST /__toolscript [{status, body}, ...]          canned answers for the next tool requests, in order
import http from 'node:http';
import https from 'node:https';

const TARGET = new URL(process.env.TARGET || 'https://obscurify.ai');
const self = 'http://127.0.0.1:8093';
let rule = { after: 0, fail: 0, kind: 'error' };
let log = [];
let tools = [];
// `outage` ms of nothing working, starting at the next tool call (tooldown) or completion (chatdown).
let down = { tool: 0, chat: 0, until: 0 };
let script = [];
let toolScript = [];
let scriptedCalls = 0;
function scripted(res, r) {
  if (r.status) { res.writeHead(r.status, { 'content-type': 'text/html' }); return res.end('<html>Bad Gateway</html>'); }
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  const send = (o) => res.write(`data: ${JSON.stringify(o)}\n\n`);
  if (r.error) { send({ error: { message: r.error } }); return res.end(); }
  if (r.reasoning) send({ choices: [{ delta: { reasoning: r.reasoning } }] });
  const calls = r.tools || (r.code ? [{ name: 'run_code', args: { language: 'python', code: r.code } }] : null);
  if (r.text && (calls || r.tools)) send({ choices: [{ delta: { content: r.text } }] });
  if (calls) {
    calls.forEach((c, index) => {
      const id = `script_${++scriptedCalls}`;
      send({ choices: [{ delta: { tool_calls: [{ index, id, type: 'function', function: { name: c.name, arguments: JSON.stringify(c.args) } }] } }] });
    });
    send({ choices: [{ delta: {}, finish_reason: 'tool_calls' }] });
  } else {
    send({ choices: [{ delta: { content: r.text } }] });
    send({ choices: [{ delta: {}, finish_reason: 'stop' }] });
  }
  send({ choices: [], usage: { prompt_tokens: 10, completion_tokens: 5, cost: 0, ...r.usage } });
  res.write('data: [DONE]\n\n');
  res.end();
}
// The next paid call really runs, but its answer never reaches the app.
let losecall = 0;
let toolbusy = 0;
let toolfail = 0;
let context = 0;
let sandboxdown = 0;
let silent = 0;
// The next N answers get one byte changed (a digit of `created`): the JSON still reads, but a TEE
// receipt no longer matches the bytes.
let teeflip = 0;
// Every path asked for, for tests that check a call was made (Stop's cancel).
let paths = [];

// `rewrite(json)` changes a JSON answer before it reaches the app.
function forward(req, res, body, onChunk, onEnd, rewrite) {
  const headers = { ...req.headers, host: TARGET.host, 'accept-encoding': 'identity' };
  if (headers.origin) headers.origin = TARGET.origin;
  const up = (TARGET.protocol === 'http:' ? http : https).request({ host: TARGET.hostname, port: TARGET.port || undefined, path: req.url, method: req.method, headers }, (r) => {
    const h = { ...r.headers };
    // Pages are served from here in the test, so their CSP has to allow here.
    for (const k of Object.keys(h)) if (k.toLowerCase() === 'content-security-policy') h[k] = String(h[k]).replaceAll(TARGET.origin, self);
    if (rewrite) {
      const parts = [];
      r.on('data', (c) => parts.push(c));
      r.on('end', () => {
        let out = Buffer.concat(parts);
        try { out = Buffer.from(JSON.stringify(rewrite(JSON.parse(out.toString())))); } catch (_) { /* not JSON: as it is */ }
        delete h['content-length'];
        res.writeHead(r.statusCode, h);
        res.end(out);
      });
      return;
    }
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
    toolbusy = +url.searchParams.get('toolbusy') || 0;
    toolfail = +url.searchParams.get('toolfail') || 0;
    context = +url.searchParams.get('context') || 0;
    sandboxdown = +url.searchParams.get('sandboxdown') || 0;
    silent = +url.searchParams.get('silent') || 0;
    teeflip = +url.searchParams.get('teeflip') || 0;
    toolScript = [];
    log = [];
    paths = [];
    tools = [];
    return res.end('ok');
  }
  if (url.pathname === '/__script') {
    const c = [];
    req.on('data', (d) => c.push(d));
    req.on('end', () => { script = JSON.parse(Buffer.concat(c).toString()); res.end('ok'); });
    return;
  }
  if (url.pathname === '/__toolscript') {
    const c = [];
    req.on('data', (d) => c.push(d));
    req.on('end', () => { toolScript = JSON.parse(Buffer.concat(c).toString()); res.end('ok'); });
    return;
  }
  if (url.pathname === '/__log') return res.end(JSON.stringify(log));
  if (url.pathname === '/__paths') return res.end(JSON.stringify(paths));
  paths.push(req.method + ' ' + url.pathname);
  if (sandboxdown && url.pathname.startsWith('/sandbox/')) { res.writeHead(500, { 'content-type': 'text/html' }); return res.end('down'); }
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
      if (toolScript.length) {
        const a = toolScript.shift();
        res.writeHead(a.status || 200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify(a.body));
      }
      if (toolbusy && url.pathname.endsWith('/call')) {
        const busy = toolbusy < 0 || toolbusy-- > 1;
        res.writeHead(busy ? 429 : 200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify(busy ? { error: 'Rate limit exceeded. Try again in 30 seconds.' } : { ok: true, status: 200, output: {}, cost_usd: 0.02, balance_usd: 3 }));
      }
      // The server is up but the tool keeps failing (a web search's 503).
      if (toolfail !== 0) {
        if (toolfail > 0) toolfail--;
        res.writeHead(503, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'Web search failed. Try again.' }));
      }
      if (losecall > 0 && url.pathname.endsWith('/call')) {
        losecall--;
        return forward(req, res, body, () => false, () => req.socket.destroy());
      }
    }
    if (context && url.pathname === '/models') {
      // A small context everywhere, so a short chat already needs compacting.
      return forward(req, res, body, null, null, (json) => {
        for (const m of json.data || []) m.context_length = context;
        return json;
      });
    }
    if (!/chat\/completions$/.test(url.pathname)) return forward(req, res, body);
    const sent = JSON.parse(body.toString());
    log.push(sent);
    // A connection that died without closing: the answer starts, and nothing more ever comes.
    if (silent > 0 && sent.stream !== false) { silent--; res.writeHead(200, { 'content-type': 'text/event-stream' }); return res.flushHeaders(); }
    // Summary requests (not streamed) always go to the server; scripts answer the chat.
    if (script.length && sent.stream !== false) return scripted(res, script.shift());
    if (teeflip > 0) {
      teeflip--;
      let flipped = false;
      return forward(req, res, body, (c) => {
        const at = flipped ? -1 : c.indexOf('"created":');
        if (at >= 0 && c[at + 10] >= 0x30 && c[at + 10] <= 0x39) { c[at + 10] = c[at + 10] === 0x31 ? 0x32 : 0x31; flipped = true; }
        return true;
      });
    }
    if (rule.after > 0) { rule.after--; return forward(req, res, body); }
    if (rule.fail <= 0) return forward(req, res, body);
    rule.fail--;
    if (rule.kind === 'hang') return;
    if (rule.kind === 'freeze') {
      let lines = 0, frozen = false;
      res.end = () => res;  // the upstream ending doesn't end this one either
      return forward(req, res, body, (c) => {
        if (frozen) return false;
        for (const line of c.toString().split(/(?<=\n)/)) {
          res.write(line);
          if (/"content":"[^"]/.test(line) && ++lines >= 3) { frozen = true; return false; }
        }
        return false;
      });
    }
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
