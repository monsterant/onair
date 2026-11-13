// 방송 제목 검색 시험 (#96): 검색 칸에 치면 제목에 그 글자가 든 방송만, 없으면 안내, 지우면 다시 전체
// node search.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(200); }
  return false;
}
const titles = (p) => p.$$eval('.list .card strong', (ls) => ls.map((l) => l.textContent)).catch(() => []);
const listText = (p) => p.$eval('.list', (e) => e.textContent ?? '').catch(() => '');

// 다른 시험 방송과 섞이지 않게 이번 실행만의 글자를 붙인다
const tag = `찾기${Date.now().toString(36)}`;
const ids = [];
let p;
try {
  p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  for (const [t, category] of [[`${tag} 노래`, 'music'], [`${tag} 게임`, 'game']]) {
    const b = await p.evaluate(async (title, category) => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title, category }) })).json(), t, category);
    ids.push(b.id);
  }
  await p.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.search');
  await p.type('.search', tag);
  check('typing shows only matching broadcasts', await waitFor(async () => { const t = await titles(p); return t.length === 2 && t.every((x) => x.includes(tag)); }, 5000), JSON.stringify(await titles(p)));
  await p.type('.search', ' 노래');
  check('narrower search', await waitFor(async () => JSON.stringify(await titles(p)) === JSON.stringify([`${tag} 노래`]), 5000), JSON.stringify(await titles(p)));
  // 10초마다 새로 읽어도 검색어가 유지된다
  await sleep(11_000);
  check('search kept after auto refresh', JSON.stringify(await titles(p)) === JSON.stringify([`${tag} 노래`]), JSON.stringify(await titles(p)));
  await p.$eval('.search', (i) => (i.value = ''));
  await p.type('.search', `${tag}없음`);
  check('no result message', await waitFor(async () => (await listText(p)).includes('찾는 방송이 없습니다'), 5000), await listText(p));
  // 분류(#113): 검색어를 그대로 두고 음악을 누르면 음악만, 전체를 누르면 다시 둘 다
  await p.$eval('.search', (i) => { i.value = ''; i.dispatchEvent(new Event('input')); });
  await p.type('.search', tag);
  await waitFor(async () => (await titles(p)).length === 2, 5000);
  await p.click('.category-filter[data-category="music"]');
  check('category filter shows only music', await waitFor(async () => JSON.stringify(await titles(p)) === JSON.stringify([`${tag} 노래`]), 5000), JSON.stringify(await titles(p)));
  check('card shows category label', (await listText(p)).includes('음악'));
  await p.click('.category-filter[data-category=""]');
  check('all category shows both again', await waitFor(async () => (await titles(p)).length === 2, 5000), JSON.stringify(await titles(p)));
  // 지우면 다시 전체 목록
  await p.$eval('.search', (i) => { i.value = ''; i.dispatchEvent(new Event('input')); });
  check('clearing shows all again', await waitFor(async () => (await titles(p)).length >= 2, 5000), JSON.stringify(await titles(p)));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (p) for (const id of ids) await p.evaluate(async (i) => fetch(`/api/broadcasts/${i}`, { method: 'DELETE' }), id).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
