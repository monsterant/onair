// 콘솔 오류 시험 (#134): 로그인하지 않은 방문자가 둘러볼 때 개발자 도구에 오류(401, favicon 404 등)가 찍히지 않는다.
// node console.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });

let owner; let bid;
try {
  // 방송 상세를 보려고 다른 창(테스트 계정)에서 방송을 하나 만든다. 둘러보는 쪽은 로그인하지 않은 창이다
  owner = await (await browser.createBrowserContext()).newPage();
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  bid = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '콘솔 시험' }) })).json())).id;

  const p = await (await browser.createBrowserContext()).newPage();
  const errors = [];
  p.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
  p.on('pageerror', (e) => errors.push(String(e)));
  p.on('response', (r) => { if (r.status() >= 400) errors.push(`${r.status()} ${r.url()}`); });
  await p.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  // 다시보기가 하나라도 있으면 그 상세도 연다
  const rec = await p.evaluate(async () => (await (await fetch('/api/recordings')).json())[0]?.id).catch(() => undefined);
  // 목록 → 방송 상세 → 다시보기 → (다시보기 상세) → 로그인 화면 → 목록. 화면을 바꿀 때마다 로그인 상태를 묻는다
  const hashes = [`#/b/${bid}`, '#/recordings', ...(rec ? [`#/r/${rec}`] : []), '#/login', '#/'];
  for (const hash of hashes) {
    await p.goto(`${BASE}/${hash}`, { waitUntil: 'networkidle2' });
    // 주소의 # 뒤만 바뀌면 페이지를 다시 읽지 않아 바로 끝날 수 있다. 화면이 부르는 요청이 잠잠해질 때까지 기다린다
    await p.waitForNetworkIdle({ idleTime: 500, timeout: 10000 }).catch(() => {});
  }
  check(`visited ${hashes.length + 1} screens (recording detail: ${rec ? 'yes' : 'no recordings'})`, true);
  // 없는 파일도 index.html이 200으로 나갈 수 있어(Caddy try_files) 응답 종류까지 본다(#134 리뷰)
  const icon = await p.evaluate(async () => {
    const r = await fetch(document.querySelector('link[rel=icon]')?.href ?? '/favicon.ico');
    return `${r.status} ${r.headers.get('content-type')}`;
  });
  check('favicon is served as svg', /^200 image\/svg/.test(icon), icon);
  check('no console errors while browsing logged out', errors.length === 0, errors.join(' | '));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
