// End-to-end test of file attachments: the app reads PDFs, spreadsheets, Word, PowerPoint and
// text files on the phone and sends their text with the message. Build a test APK against a
// server, install it on a running emulator, then: ACCOUNT=<account number> node e2e/files.mjs
// (PKG=ai.obscurify.play for the Play build). Each fixture hides a code word the model must find.
import { launch, ok, shot } from './lib.mjs';

const fx = new URL('./fixtures/', import.meta.url).pathname;
const { page } = await launch(true);
const toast = async (text, ms = 30000) => page.waitForSelector(`.toast:has-text(${JSON.stringify(text)})`, { timeout: ms });

await page.locator('.login input').fill(process.env.ACCOUNT);
await page.getByRole('button', { name: 'Sign In' }).click();
await page.waitForSelector('#composer', { timeout: 30000 });
await page.locator('.title.model').click();
await page.locator('input.search').fill('Llama 3.3 70b Instruct');
await page.locator('.pick', { hasText: 'Meta: Llama 3.3 70b Instruct' }).first().locator('.grow').click();

ok(await page.locator('.chip.doc', { hasText: 'File' }).count() === 1, 'a File button next to Image');
const attach = async (names) => {
  await page.setInputFiles('#pickdoc', names.map(n => fx + n));
  await page.waitForFunction(n => document.querySelectorAll('.docs .chip').length === n, names.length, { timeout: 60000 });
  const chips = await page.locator('.docs .chip').allTextContents();
  for (const n of names) ok(chips.some(c => c.includes(n)), `${n} read and attached`);
};

// Errors: an image-only PDF and a binary file are refused with a reason, not sent.
await page.setInputFiles('#pickdoc', [fx + 'scanned.pdf']);
await toast('no text to read');
ok(true, 'a scanned PDF is refused: attach it as images instead');
await page.setInputFiles('#pickdoc', [fx + 'photo.png']);
await toast('not this kind of file');
ok(await page.locator('.docs .chip').count() === 0, 'a binary file is refused and nothing is attached');

// Four files in one message: PDF, CSV, Excel, Word.
await attach(['report.pdf', 'sales.csv', 'budget.xlsx', 'memo.docx']);
ok(await page.locator('.docs .chip').count() === 4, 'four files wait above the composer');
shot('files-composer');
await page.locator('#composer').fill('Each attached file contains one code word in capital letters. List all of them, comma separated, nothing else.');
await page.locator('.send').click();
await page.waitForSelector('.msg.assistant .meta', { timeout: 180000 });
let answer = (await page.locator('.msg.assistant').last().textContent()).toUpperCase();
for (const w of ['PERIWINKLE', 'TANGERINE', 'MARIGOLD', 'CERULEAN']) ok(answer.includes(w), `model read ${w}`);
ok(await page.locator('.msg.user .doc').count() === 4, 'the message shows its four files');
shot('files-answer');

// PowerPoint, OpenDocument, Markdown, and a file too long to send whole.
await attach(['deck.pptx', 'budget.ods', 'memo.odt', 'notes.md']);
await page.locator('#composer').fill('List every code word in capital letters that appears in these four files, comma separated, nothing else.');
await page.locator('.send').click();
await page.waitForFunction(() => document.querySelectorAll('.msg.assistant').length >= 2 && !document.querySelector('.send.stop'), null, { timeout: 180000 });
answer = (await page.locator('.msg.assistant').last().textContent()).toUpperCase();
for (const w of ['SAFFRON', 'MARIGOLD', 'CERULEAN', 'VERMILION']) ok(answer.includes(w), `model read ${w}`);

await attach(['big.txt']);
await page.locator('#composer').fill('Reply with only the first four words of the attached file.');
await page.locator('.send').click();
await page.waitForFunction(() => document.querySelectorAll('.msg.assistant').length >= 3 && !document.querySelector('.send.stop'), null, { timeout: 180000 });
ok((await page.locator('.msg.user .doc').last().textContent()).includes('(cut)'), 'a long file is cut and says so');
ok((await page.locator('.msg.assistant').last().textContent()).toUpperCase().includes('START OF BIG'), 'the cut file still reaches the model');

// Chats are on the phone: the files come back after a restart.
const again = (await launch(false)).page;
await again.waitForSelector('#composer', { timeout: 20000 });
ok(await again.locator('.msg.user .doc').count() === 9, 'files are kept with the chat after a restart');
console.log('all passed');
process.exit(0);
