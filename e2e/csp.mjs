// 스크립트 출처 제한(CSP) 시험 (#136): 운영 화면에서 CSP 위반 보고·차단이 없는지 본다.
// 목록 → 방송 상세(플레이어) → 다시보기 상세 → 결제위젯을 열고, 콘솔에 CSP 관련 오류가 있으면 실패.
// CSP 헤더는 운영 Caddy가 붙인다. 로컬 개발 서버(Vite)에는 없어서 "헤더 없음"만 알리고 나머지를 본다.
// node csp.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const violations = [];
const watch = (p) => {
  p.on('console', (m) => {
    if (/Content.Security.Policy/i.test(m.text())) violations.push(m.text());
  });
  p.on('pageerror', (e) => {
    if (/Content.Security.Policy/i.test(String(e))) violations.push(String(e));
  });
};
const guest = async () => {
  const p = await (await browser.createBrowserContext()).newPage();
  watch(p);
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
};

let owner; let bid;
try {
  owner = await guest();
  // 어떤 CSP 헤더가 붙는지 알린다(로컬은 없음)
  const headers = await owner.evaluate(async () => {
    const r = await fetch(location.origin + '/');
    return { enforce: r.headers.get('content-security-policy'), report: r.headers.get('content-security-policy-report-only') };
  });
  results.push(`INFO csp=${headers.enforce} | report-only=${headers.report}`);
  bid = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: 'CSP 시험' }) })).json())).id;

  const d = await guest();
  await d.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  await d.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await sleep(1000);
  const rec = await d.evaluate(async () => (await (await fetch('/api/recordings')).json())[0]?.id).catch(() => undefined);
  if (rec) {
    await d.goto(`${BASE}/#/r/${rec}`, { waitUntil: 'networkidle2' });
    await d.waitForSelector('video', { timeout: 10000 }).catch(() => {});
    await sleep(2000);
  }
  check(`visited list, detail, replay (${rec ? 'yes' : 'no recordings'})`, true);
  // 결제위젯(토스 SDK 스크립트)
  await d.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  const canDonate = await d.waitForSelector('.donations-panel select', { timeout: 10000 }).then(() => true).catch(() => false);
  if (canDonate) {
    await d.click('.donations-panel button');
    const widget = await d.waitForFunction(() => [...document.querySelectorAll('.donations-panel button')].some((b) => b.textContent === '결제하기' && !b.hidden), { timeout: 20000 }).then(() => true).catch(() => false);
    check('toss widget rendered', widget);
    await sleep(1500);
  } else {
    results.push('INFO donations off (no toss keys): widget not checked');
  }
  check('no CSP violations', violations.length === 0, violations.join(' | '));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
