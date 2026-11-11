// Answers appear as they're written, not all at once at the end: Code Mode (Tools and Code on) and plain
// chat (both off). Samples the live answer while it streams. Real model, any server:
//   ACCOUNT=<funded number> [MODEL="Anthropic: Claude Sonnet 5.5"] [ONLY=code|plain] node e2e/stream.mjs
// Pick a model whose provider streams: some (GLM 5.3 on one route) send the whole answer as one chunk.
import { launch, agentSwitches, ok, sleep } from './lib.mjs';

const MODEL = process.env.MODEL || 'Anthropic: Claude Sonnet 5.5';
let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { state: 'attached', timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill(MODEL.split(': ').pop());
await page.locator('.pick', { hasText: MODEL }).first().locator('.grow').click();

// How long the live answer was, every 100 ms, until the reply is done.
const watch = async (text) => {
  await sleep(400);
  await page.locator('#composer').fill(text);
  await sleep(400);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 });
  return page.evaluate(() => new Promise((done) => {
    const seen = [];
    const t = setInterval(() => {
      const live = document.querySelector('.msg.assistant .cursor');
      seen.push(live ? live.textContent.length : -1);
      if (!document.querySelector('.send.stop')) { clearInterval(t); done(seen); }
    }, 100);
  }));
};
const lengths = (seen) => [...new Set(seen.filter((n) => n > 0))];
const PROMPT = 'Write about 250 words on how tides work. Plain prose, no tools.';

for (const tools of process.env.ONLY ? [process.env.ONLY === 'code'] : [true, false]) {
  await agentSwitches(page, tools);
  const seen = await watch(PROMPT);
  const steps = lengths(seen);
  const final = (await page.locator('.msg.assistant').last().textContent()) || '';
  ok(final.length > 600, `${tools ? 'Code Mode' : 'plain chat'}: a whole answer came back (${final.length} chars)`);
  ok(steps.length >= 5, `${tools ? 'Code Mode' : 'plain chat'}: it grew on screen as it streamed (${steps.length} sizes: ${steps.slice(0, 12).join(' ')})`);
  await page.locator('.bar .icon').last().click();
  await sleep(300);
}
console.log('all ok');
process.exit(0);
