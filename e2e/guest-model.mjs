// A guest is told the model that answers them: the server's guest model (DeepSeek V4 Flash), in the
// title bar and beside the answer. The app used to name Llama 3.3, the guest model before
// 2026-11-13, while the server answered with DeepSeek.
// Test APK against a dev server whose DEFAULT_FREE_MODEL is deepseek/deepseek-v4-flash, as on
// obscurify.ai: node e2e/guest-model.mjs
import { launch, ok, shot } from './lib.mjs';

const answered = (page, n) => page.waitForFunction((n) => document.querySelectorAll('.msg.assistant').length >= n && !document.querySelector('.send.stop'), n, { timeout: 150000 });

const { page } = await launch(true);
await page.getByRole('button', { name: 'Chat as a guest' }).click();
await page.waitForSelector('#composer');
const title = await page.locator('.title.model').textContent();
ok(/DeepSeek V4 Flash/.test(title) && !/Llama/i.test(title), 'the title names the guest model: ' + title.trim());

await page.locator('#composer').fill('Say hi in one word.');
await page.locator('.send').click();
await answered(page, 1);
const answer = await page.locator('.msg.assistant').last().textContent();
ok(/DeepSeek V4 Flash/.test(answer) && !/Llama/i.test(answer), 'the answer names the model that wrote it: ' + answer.slice(-60).replace(/\s+/g, ' '));
shot('guest-model');
console.log('all passed');
process.exit(0);
