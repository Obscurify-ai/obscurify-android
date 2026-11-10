// Specialist categories: the model picker offers the use cases the server tags models with
// (Writing, Security, Translation...) and each one lists only its models.
// Test APK against a dev server, then: node e2e/categories.mjs
import { launch, ok, shot } from './lib.mjs';

// A new (unfunded) account: guests have one model and no picker.
const { page } = await launch(true);
await page.getByText('Create one').click();
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Generate Account Number' }).click();
await page.locator('.number').waitFor({ timeout: 20000 });
await page.locator('.check input').check();
await page.getByRole('button', { name: 'Start Chatting' }).click();
await page.waitForSelector('#composer');
await page.locator('.title.model').click();
const chips = await page.locator('.toggles:not(.providers) .chip').allTextContents();
for (const c of ['Zero retention', 'TEE', 'Writing', 'Roleplay', 'Security', 'Translation', 'Languages']) ok(chips.some(t => t.startsWith(c)), `${c} is offered`);
await page.locator('.toggles:not(.providers) .chip', { hasText: /^Translation/ }).click();
const rows = await page.locator('.pick .grow > div:first-child').allTextContents();
ok(rows.length > 0 && rows.every(r => /Hy-MT/i.test(r)), 'Translation lists only translation models: ' + rows.join(', '));
await page.locator('.toggles:not(.providers) .chip', { hasText: /^Writing/ }).click();
ok(await page.locator('.pick', { hasText: /Hemingway/i }).count() > 0, 'Writing lists Hemingway');
await page.locator('.toggles:not(.providers) .chip', { hasText: /^TEE/ }).click();
const tee = await page.locator('.pick').allTextContents();
ok(tee.length > 0 && tee.every(r => r.includes('· TEE')), 'every TEE model is labelled TEE');
shot('categories');
process.exit(0);
