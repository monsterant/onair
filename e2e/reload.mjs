// 송출 중 새로고침(#31 P14): 페이지를 떠나면 바로 "대기"로 돌아가고, 곧바로 다시 송출해도 409가 나지 않는다.
// node reload.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--auto-select-desktop-capture-source=Entire screen'],
});
let page; let bid;
const status = () => page.evaluate(async (id) => (await (await fetch(`/api/broadcasts/${id}`)).json()).status, bid);
async function goLive() {
  await page.waitForSelector('.start-screen:not([disabled])');
  await page.click('.start-screen');
  for (let i = 0; i < 25; i++) {
    await sleep(1000);
    if ((await status()) === 'live') return true;
  }
  return false;
}
try {
  page = await (await browser.createBrowserContext()).newPage();
  page.on('dialog', (d) => d.accept());
  await page.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await page.click('.demo .row button');
  await sleep(1000);
  bid = await page.evaluate(async () => (await (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '새로고침 시험' }) })).json()).id);
  await page.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  for (let round = 1; round <= 3; round++) {
    check(`round ${round}: live`, await goLive());
    const t0 = Date.now();
    await page.reload({ waitUntil: 'networkidle2' });
    let ready = false;
    for (let i = 0; i < 10 && !ready; i++) {
      ready = (await status()) === 'ready';
      if (!ready) await sleep(500);
    }
    const secs = ((Date.now() - t0) / 1000).toFixed(1);
    check(`round ${round}: ready soon after reload (${secs}s)`, ready && Date.now() - t0 < 6000, secs);
  }
  // 마지막으로 바로 다시 송출해도 된다
  check('restart right after reload goes live', await goLive());
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (page && bid) await page.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
