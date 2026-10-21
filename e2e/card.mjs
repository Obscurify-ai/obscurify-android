// Card payment (Stripe Elements in the app's WebView, Stripe test mode on dev) and an image
// attachment read by a vision model. Run after app.mjs, which leaves the app signed in and funded.
import { launch, ok, shot, device, adb, sleep } from './lib.mjs';

const { page } = await launch(false);
await page.waitForSelector('#composer', { timeout: 20000 });

// 1. Tapping Image opens Android's photo picker. (Dioxus swallows clicks on file inputs it renders,
// which once left this button dead.)
await page.locator('.chip.file').click();
await sleep(2500);
const top = adb('shell', 'dumpsys', 'activity', 'activities').split('\n').find(l => l.includes('topResumedActivity')) || '';
ok(/photopicker|documentsui/i.test(top), 'Image opens the system photo picker');
adb('shell', 'input', 'keyevent', '4');
await sleep(1500);

// 2. An image goes out with the prompt and the model sees it. (The picker itself is Android UI, so
// the test hands the bridge a picked file the way the picker would.)
await page.locator('.title.model').click();
await page.locator('input.search').fill('GPT-4o mini');
await page.locator('.pick', { hasText: 'OpenAI: GPT-4o mini' }).first().locator('.grow').click();
await page.evaluate(async () => {
  const c = document.createElement('canvas'); c.width = c.height = 200;
  const g = c.getContext('2d'); g.fillStyle = '#ff0000'; g.fillRect(0, 0, 200, 200);
  const blob = await new Promise(r => c.toBlob(r, 'image/png'));
  const input = document.getElementById('pick'), dt = new DataTransfer();
  dt.items.add(new File([blob], 'red.png', { type: 'image/png' }));
  input.files = dt.files;
  input.dispatchEvent(new Event('change', { bubbles: true }));
});
await page.waitForSelector('.thumbs img', { timeout: 10000 });
ok(true, 'picked image shown before sending');
await page.locator('#composer').fill('What single colour fills this image? One word.');
await page.locator('.send').click();
await page.waitForSelector('.msg.user img', { timeout: 10000 });
await page.waitForFunction(() => [...document.querySelectorAll('.msg.assistant, .msg.err')].some(m => m.querySelector('.meta')), null, { timeout: 90000 });
const answer = await page.locator('.msg.assistant, .msg.err').last().textContent();
ok(/red/i.test(answer), 'vision model saw the image: ' + answer.slice(0, 40));
shot('image');

// 3. Card: the Stripe form loads in the app, a test card pays, and the order becomes vouchers.
await page.locator('.bar .icon').first().click();
await page.locator('.drawer .foot button').click();
const before = parseFloat((await page.locator('.big').textContent({ timeout: 20000 })).slice(1)) || 0;
await page.getByText('Add funds').click();
await page.locator('input.field').fill('3');
await page.getByText('Continue').click();
const stripe = page.frameLocator('#stripe iframe').first();
await stripe.locator('input[name="number"]').fill('4242424242424242', { timeout: 45000 });
await stripe.locator('input[name="expiry"]').fill('12/34');
await stripe.locator('input[name="cvc"]').fill('123');
const zip = stripe.locator('input[name="postalCode"]');
if (await zip.count()) await zip.fill('33101');
shot('card');
await page.locator('button.go', { hasText: 'Pay $3.00' }).click();
await page.waitForSelector('text=waiting to be added', { timeout: 120000 });
ok(true, 'card payment turned into vouchers');
await page.getByText('Add now').click();
await page.waitForSelector('.toast', { timeout: 30000 });
await page.locator('.bar .icon').first().click();
await page.waitForFunction(b => parseFloat(document.querySelector('.big')?.textContent.slice(1)) >= b + 2.99, before, { timeout: 30000 });
ok(true, `balance went up by the $3 card payment (was $${before.toFixed(2)})`);

console.log('all passed');
await device.close();
