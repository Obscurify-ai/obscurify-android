// Shared by the e2e scripts: drives the installed app's WebView with Playwright's Android support.
import { _android } from 'playwright';
import { execFileSync } from 'node:child_process';

export const PKG = 'ai.obscurify.android';
export const adb = (...a) => execFileSync('adb', a, { encoding: 'utf8' }).trim();
export const sleep = ms => new Promise(r => setTimeout(r, ms));
export const ok = (cond, what) => { if (!cond) throw new Error('FAILED: ' + what); console.log('ok -', what); };
export const shot = name => execFileSync('sh', ['-c', `adb exec-out screencap -p > /tmp/obs-e2e-${name}.png`]);
// ANDROID_SERIAL picks the device when several are attached (adb reads it too).
export const device = (await _android.devices()).find(d => !process.env.ANDROID_SERIAL || d.serial() === process.env.ANDROID_SERIAL);

/** Restarts the app (wiping its data with `clear`) and returns its page. */
export async function launch(clear) {
  adb('shell', 'am', 'force-stop', PKG);
  if (clear) adb('shell', 'pm', 'clear', PKG);
  adb('shell', 'am', 'start', '-n', `${PKG}/dev.dioxus.main.MainActivity`);
  let pid = 0, webview;
  for (let i = 0; i < 60 && !webview; i++) {
    await sleep(500);
    try { pid = +adb('shell', 'pidof', PKG); } catch {}
    webview = device.webViews().find(w => w.pkg() === PKG && w.pid() === pid);
  }
  if (!webview) throw new Error("the app's WebView never appeared");
  const page = await webview.page();
  await page.waitForSelector('#main > *', { timeout: 15000 });
  return { browser: { close: async () => {} }, page };
}
