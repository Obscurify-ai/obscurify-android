// A chat goes on past the model's context window: earlier messages are summarized in chunks and
// the latest message goes as it is (compact.rs), as on the web.
//  1. Plain chat compacts before sending once the history is near the context, with a summary
//     that keeps a code word from the first message.
//  2. A "context length exceeded" refusal compacts and sends the same message again.
// The proxy lists every model with an 8K context, so a short chat is already "long"; the chat's
// answers are scripted, the summaries come from the real model.
// With flaky-proxy.mjs running (see there): ACCOUNT=<funded number> node e2e/compaction.mjs
import { launch, agentSwitches, ok, sleep } from './lib.mjs';

const ctl = (q) => fetch('http://127.0.0.1:8093/' + q).then((r) => r.text());
const script = (s) => fetch('http://127.0.0.1:8093/__script', { method: 'POST', body: JSON.stringify(s) });
const log = async () => JSON.parse(await ctl('__log'));

await ctl('__set?context=8000');
let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('.tools-switch', { timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('DeepSeek V4 Flash');
await page.locator('.pick', { hasText: 'DeepSeek: DeepSeek V4 Flash' }).first().locator('.grow').click();
// Plain chat: Tools and Code off.
await agentSwitches(page, false);

const say = async (text) => {
  await page.locator('#composer').fill(text);
  await sleep(300);
  await page.locator('.send').click();
  await page.waitForSelector('.send.stop', { timeout: 10000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector('.send.stop'), null, { timeout: 300000 });
};
const filler = 'The expedition logbook describes the weather, the route and the supplies in great detail. '.repeat(30);

// 1. Six long exchanges; the code word is in the first.
await script(Array.from({ length: 8 }, (_, i) => ({ text: `Noted day ${i + 1}. ${filler}` })));
for (let i = 0; i < 6; i++) {
  await say(`Day ${i + 1} notes.${i === 0 ? ' Remember this: my secret code word is PERIWINKLE-42.' : ''} ${filler}`);
}
let sent = await log();
const summaries = sent.filter((b) => b.stream === false);
const lastChat = sent.filter((b) => b.stream !== false).at(-1);
ok(summaries.length >= 1, `earlier messages were summarized (${summaries.length} summary requests)`);
ok(/Summary of the conversation so far/.test(JSON.stringify(lastChat.messages[0])), 'the request opens with the summary');
ok(/PERIWINKLE\W{1,6}42/.test(JSON.stringify(lastChat.messages[0])), 'the summary kept the code word');
ok(JSON.stringify(lastChat.messages).length / 3 < 8000, 'the request fits the context');
ok(await page.locator('.compacted').count() > 0, 'the chat says it compacted');

// 2. Refused as too long: compact and send again.
await ctl('__set?context=8000');
await script([
  { error: 'Context length exceeded: this conversation is about 9K tokens and deepseek/deepseek-v4-flash takes up to 8K. Compact it or start a new chat.' },
  { text: 'done' },
]);
await say('Reply with just the word: done');
sent = await log();
ok(sent.filter((b) => b.stream === false).length >= 1, 'it compacted after the refusal');
ok(sent.filter((b) => b.stream !== false).length === 2, 'and sent the message again');
ok(/done/.test(await page.locator('.msg.assistant').last().textContent()), 'the answer came');

console.log('compaction: all passed');
process.exit(0);
