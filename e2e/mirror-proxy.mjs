// Two local doors to one server, for mirror.mjs: a "primary" (the app's server, 8094) and a "mirror"
// (8095). Both pass everything to TARGET, and both answer GET /api/mirrors themselves with the mirror,
// as the server does for a Pro account. Build the test APK with OBSCURIFY_API=http://127.0.0.1:8094 and
// `adb reverse tcp:8094 tcp:8094; adb reverse tcp:8095 tcp:8095`.
//   GET /__down (on the mirror port)   the primary stops listening: connections to it are refused
//   GET /__up                          it listens again
//   GET /__seen                        the paths the mirror has passed on, in order
import http from 'node:http';
import https from 'node:https';

const TARGET = new URL(process.env.TARGET || 'https://obscurify.ai');
const MIRROR = 'http://127.0.0.1:8095';
const seen = [];

function pass(req, res, door) {
  if (req.url === '/api/mirrors') {
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({ mirrors: [MIRROR] }));
  }
  if (door === 'mirror') seen.push(req.method + ' ' + req.url.split('?')[0]);
  const headers = { ...req.headers, host: TARGET.host };
  const up = https.request({ host: TARGET.hostname, port: TARGET.port || 443, path: req.url, method: req.method, headers }, (r) => {
    res.writeHead(r.statusCode, r.headers);
    r.pipe(res);
  });
  up.on('error', () => { res.writeHead(502); res.end(); });
  req.pipe(up);
}

let primary;
const sockets = new Set();
function up() {
  if (primary?.listening) return;
  primary = http.createServer((q, s) => pass(q, s, 'primary'));
  primary.on('connection', (c) => { sockets.add(c); c.on('close', () => sockets.delete(c)); });
  primary.listen(8094, '127.0.0.1');
}
up();
http.createServer((req, res) => {
  if (req.url === '/__down') { primary.close(); for (const c of sockets) c.destroy(); res.end('down'); return; }
  if (req.url === '/__up') { up(); res.end('up'); return; }
  if (req.url === '/__seen') { res.end(JSON.stringify(seen)); return; }
  pass(req, res, 'mirror');
}).listen(8095, '127.0.0.1');
console.log('mirror-proxy: primary :8094, mirror :8095 ->', TARGET.href);
