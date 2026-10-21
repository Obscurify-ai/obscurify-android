// Store screenshots (fastlane/metadata/android/en-US/images/phoneScreenshots) from a test build on an
// emulator, signed into a funded dev account: run after app.mjs.
import { launch, device, shot, sleep } from './lib.mjs';
import { execFileSync } from 'node:child_process';

const dir = new URL('../fastlane/metadata/android/en-US/images/phoneScreenshots/', import.meta.url).pathname;
const save = (n, name) => { shot(name); execFileSync('cp', [`/tmp/obs-e2e-${name}.png`, `${dir}${n}.png`]); };

let { page } = await launch(false);
await page.waitForSelector('#composer', { timeout: 20000 });

// A chat with a formatted answer.
await page.locator('.bar button.icon').last().click(); // new chat
await page.locator('.title.model').click();
await page.locator('input.search').fill('Llama 3.3 70b Instruct');
await page.locator('.pick', { hasText: 'Meta: Llama 3.3 70b Instruct' }).first().locator('.grow').click();
if (await page.locator('.chip.on', { hasText: 'Web search' }).count()) await page.locator('.chip', { hasText: 'Web search' }).click();
await page.locator('#composer').fill('Give me three short tips for keeping my online life private, as a numbered list with one bold phrase each.');
await page.locator('.send').click();
await page.waitForSelector('.msg.assistant .meta', { timeout: 120000 });
await page.locator('.chip', { hasText: 'Web search' }).click();
await sleep(500);
save(1, 'store-chat');

// The model picker.
await page.locator('.title.model').click();
await page.locator('.chip', { hasText: 'Frontier' }).click();
await sleep(800);
save(2, 'store-models');
await page.locator('.drawer .bar .icon').click();

// Account, then Add funds with a Bitcoin order.
await page.locator('.bar .icon').first().click();
await page.locator('.drawer .foot button').click();
await page.waitForSelector('.big');
await sleep(1500);
save(3, 'store-account');
await page.getByText('Add funds').click();
await page.getByRole('button', { name: 'Bitcoin' }).click();
await page.locator('input.field').fill('10');
await page.getByText('Continue').click();
await page.waitForSelector('.qr svg', { timeout: 30000 });
await sleep(500);
save(4, 'store-funds');

// Sign-in screen last (signs out).
await page.locator('.bar .icon').first().click(); // back to Account
await page.getByRole('button', { name: 'Sign out' }).click();
await page.getByRole('button', { name: 'Sign out' }).last().click();
await page.waitForSelector('.login input');
save(5, 'store-signin');
await device.close();
