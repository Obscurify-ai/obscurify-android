// Google Play store screenshots (play/listing/phoneScreenshots) from the Play build against production:
//   DEVTOOLS=1 ./build.sh play x86_64 && adb install -r target/obscurify-play.apk
//   ACCOUNT=<account number> node e2e/play-screenshots.mjs
// Never show prices to buy, crypto or uncensored models: the Play build has none of them anyway.
import { launch, device, shot, sleep } from './lib.mjs';
import { execFileSync } from 'node:child_process';

const dir = new URL('../play/listing/phoneScreenshots/', import.meta.url).pathname;
const save = (n, name) => { shot(name); execFileSync('cp', [`/tmp/obs-e2e-${name}.png`, `${dir}${n}.png`]); };

let { page } = await launch(true);
await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });

// A chat with a formatted answer.
await page.locator('.title.model').click();
await page.locator('input.search').fill('Llama 3.3 70b Instruct');
await page.locator('.pick', { hasText: 'Meta: Llama 3.3 70b Instruct' }).first().locator('.grow').click();
if (await page.locator('.chip.on', { hasText: 'Web search' }).count()) await page.locator('.chip', { hasText: 'Web search' }).click();
await page.locator('#composer').fill('Give me three short tips for keeping my online life private, as a numbered list with one bold phrase each.');
await page.locator('.send').click();
await page.waitForSelector('.msg.assistant .meta', { timeout: 120000 });
await sleep(800);
save(1, 'play-store-chat');

// The model picker on Frontier.
await page.locator('.title.model').click();
await page.locator('.chip', { hasText: 'Frontier' }).click();
await sleep(1000);
save(2, 'play-store-models');
await page.locator('.drawer .bar .icon').click();

// Reporting an answer.
await page.locator('.msg.assistant .meta button', { hasText: 'Report' }).last().click();
await page.locator('.report .chip', { hasText: 'Other' }).click();
await sleep(500);
save(3, 'play-store-report');
await page.locator('.report button', { hasText: 'Cancel' }).click();

// Account.
await page.locator('.bar .icon').first().click();
await page.locator('.drawer .foot button').click();
await page.waitForSelector('.big');
await sleep(1500);
save(4, 'play-store-account');

// Sign-in screen last (signs out; chats on the phone are deleted, the account isn't).
await page.getByRole('button', { name: 'Sign out' }).click();
await page.getByRole('button', { name: 'Sign out' }).last().click();
await page.waitForSelector('.login input');
await sleep(500);
save(5, 'play-store-signin');
await device.close();
process.exit(0);
