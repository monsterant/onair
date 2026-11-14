// 화면 배치 시험 (#124): 방송 주인 화면에서 시작 버튼이 첫 화면(스크롤 없이) 안에 보인다. PC 1280×800, 휴대폰 375×740.
// node layout.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });

let p; let bid;
try {
  p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  bid = (await p.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '배치 시험' }) })).json())).id;
  // 버튼의 아래 끝이 창 높이 안이면 스크롤 없이 보인다
  const bottom = (sel) => p.$eval(sel, (e) => { const r = e.getBoundingClientRect(); return Math.round(r.bottom); }).catch(() => null);
  for (const [name, vp, sel] of [
    ['pc 1280x800', { width: 1280, height: 800 }, '.start-screen'],
    ['phone 375x740', { width: 375, height: 740, isMobile: true, hasTouch: true, deviceScaleFactor: 2 }, '.start-camera'],
  ]) {
    await p.setViewport(vp);
    await p.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
    await p.waitForSelector(sel);
    await sleep(500);
    await p.evaluate(() => window.scrollTo(0, 0));
    const b = await bottom(sel);
    check(`${name}: start button visible without scrolling`, b !== null && b <= vp.height, `bottom=${b} height=${vp.height}`);
  }
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (p && bid) await p.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
