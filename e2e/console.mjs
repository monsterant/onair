// 콘솔 오류 시험 (#134): 로그인하지 않은 방문자가 둘러볼 때 개발자 도구에 오류(401, favicon 404 등)가 찍히지 않는다.
// node console.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });

try {
  const p = await (await browser.createBrowserContext()).newPage();
  const errors = [];
  p.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
  p.on('pageerror', (e) => errors.push(String(e)));
  p.on('response', (r) => { if (r.status() >= 400) errors.push(`${r.status()} ${r.url()}`); });
  // 목록 → 다시보기 → 로그인 화면 → 목록. 화면을 바꿀 때마다 로그인 상태를 묻는다
  for (const hash of ['#/', '#/recordings', '#/login', '#/']) {
    await p.goto(`${BASE}/${hash}`, { waitUntil: 'networkidle2' });
    await sleep(500);
  }
  // 브라우저가 따로 부르는 favicon도 본다
  const icon = await p.evaluate(async () => (await fetch(document.querySelector('link[rel=icon]')?.href ?? '/favicon.ico')).status);
  check('favicon is served', icon === 200, `status=${icon}`);
  check('no console errors while browsing logged out', errors.length === 0, errors.join(' | '));
} catch (e) {
  check('no exception', false, e.stack);
}
console.log(results.join('\n'));
await browser.close();
