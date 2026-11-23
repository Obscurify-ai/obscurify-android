// Math in replies, as on the web chat: TeX between $...$, $$...$$, \(...\) and \[...\] is typeset by
// the bundled KaTeX (nothing fetched); money, code and broken TeX are left as text. Guest chat with a
// scripted reply, so no model is called. Leaves a screenshot.
//   node flaky-proxy.mjs &  node math.mjs   (test APK with OBSCURIFY_API=http://127.0.0.1:8093,
//   DEVTOOLS=1, `adb reverse tcp:8093 tcp:8093`)
import { execFileSync } from 'node:child_process';
import { launch, ok, sleep } from './lib.mjs';

const SHOTS = process.env.SHOTS || '/tmp';
const script = (s) => fetch('http://127.0.0.1:8093/__script', { method: 'POST', body: JSON.stringify(s) });
const REPLY = [
  'Energy: $E = mc^2$, and the roots are \\(x = \\frac{-b \\pm \\sqrt{b^2-4ac}}{2a}\\).',
  '',
  '$$\\int_0^\\infty e^{-x^2}\\,dx = \\frac{\\sqrt{\\pi}}{2}$$',
  '',
  '\\[ \\sum_{n=1}^{\\infty} \\frac{1}{n^2} = \\frac{\\pi^2}{6} \\]',
  '',
  'It costs $5 and $10, or between $5-$10 a month. Café ünïcode \\é stays.',
  '',
  'Wide: $$' + Array.from({ length: 30 }, (_, i) => 'x_{' + i + '}').join(' + ') + '$$',
  '',
  'In code it stays: `echo $HOME $PATH` and',
  '',
  '```bash',
  'echo "$a_b and $c_d"',
  '```',
  '',
  'A slip: $\\frac{1}{$ and $\\href{javascript:alert(1)}{x}$.',
].join('\n');

const { page } = await launch(true);
await page.getByRole('button', { name: 'Chat as a guest' }).click();
await script([{ text: REPLY }]);
await page.locator('#composer').fill('Show me some math.');
await sleep(400);
await page.locator('.send').click();
await page.waitForFunction(() => document.querySelectorAll('span.math-tex').length >= 5 && !document.querySelector('.send.stop'), null, { timeout: 30000 });
for (let i = 0; i < 40 && await page.evaluate(() => !!document.querySelector('span.math-tex:not([data-typeset])')); i++) await sleep(250);
const r = await page.evaluate(() => {
  const msgs = [...document.querySelectorAll('.msg')];
  const el = msgs[msgs.length - 1];
  return {
    html: el.innerHTML,
    text: el.innerText,
    math: [...el.querySelectorAll('span.math-tex')].map((s) => ({
      display: s.classList.contains('math-display'),
      typeset: !!s.querySelector('.katex'),
      error: !!s.querySelector('.katex-error'),
      tex: s.querySelector('annotation[encoding="application/x-tex"]')?.textContent || s.textContent,
    })),
    code: [...el.querySelectorAll('code')].map((c) => c.textContent),
    width: el.scrollWidth,
    box: el.clientWidth,
    fetched: performance.getEntriesByType('resource').filter((e) => /katex/i.test(e.name)).map((e) => e.name),
  };
});
console.log(JSON.stringify(r.math));
ok(r.math.length === 7, `seven formulas found (${r.math.length})`);
ok(r.math.slice(0, 5).every((m) => m.typeset && !m.error), 'the five good formulas are typeset');
ok(JSON.stringify(r.math.slice(0, 5).map((m) => m.display)) === '[false,false,true,true,true]', '$$ and \\[ are display, $ and \\( inline');
ok(r.math[0].tex === 'E = mc^2' && r.math[2].tex.includes('\\int_0^\\infty'), 'the TeX survives markdown (backslashes, underscores)');
ok(r.text.includes('$5 and $10') && r.text.includes('$5-$10'), 'money stays text');
ok(r.text.includes('Café ünïcode'), 'text with accents around an escape is intact');
ok(r.code.includes('echo $HOME $PATH') && r.code.some((c) => c.includes('echo "$a_b and $c_d"')), 'code keeps its dollars');
ok(r.text.includes('\\frac{1}{'), 'broken TeX shows as text');
ok(!/<a\b[^>]*javascript|href="javascript/i.test(r.html), 'links in TeX are not made');
ok(!r.html.includes('OBSCURIFYMATH'), 'no math token is left in the reply');
ok(r.width <= r.box + 1, `a wide formula scrolls inside the reply (${r.width} <= ${r.box})`);
ok(r.fetched.length === 0, 'KaTeX is not fetched from the network');
execFileSync('sh', ['-c', `adb exec-out screencap -p > ${SHOTS}/math.png`]);
process.exit(0);
