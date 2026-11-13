// 도네이션 화면 시험 (#41). 실제 결제(결제창)는 자동 브라우저에서 넘어가지 않아 사람이 따로 확인한다.
// node donate.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const ctx = async () => (await browser.createBrowserContext()).newPage();
const guest = async () => { const p = await ctx(); await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' }); await p.click('.demo .row button'); await sleep(1000); return p; };
const panelText = (p) => p.$eval('.donations-panel', (x) => x.innerText);
let owner; let bid;
try {
  owner = await guest();
  bid = await owner.evaluate(async () => (await (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '후원 시험' }) })).json()).id);
  await owner.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.donations-panel');
  await owner.waitForFunction(() => document.querySelector('.donation-total')?.textContent?.includes('건'), { timeout: 10000 }).catch(() => {});
  check('total shows 0', (await panelText(owner)).includes('0원 · 0건'), await panelText(owner));
  check('owner cannot donate', (await panelText(owner)).includes('내 방송에는 후원할 수 없습니다') && (await owner.$('.donations-panel select')) === null);

  const anon = await ctx();
  await anon.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await anon.waitForSelector('.donations-panel');
  check('anonymous sees login hint', (await panelText(anon)).includes('로그인') && (await anon.$('.donations-panel select')) === null);

  const d = await guest();
  await d.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await d.waitForSelector('.donations-panel select');
  check('amount choices', (await d.$$eval('.donations-panel option', (o) => o.map((x) => x.value))).join() === '1000,5000,10000,50000');
  await d.select('.donations-panel select', '5000');
  await d.type('.donations-panel input', '응원합니다');
  await d.click('.donations-panel button');
  await d.waitForFunction(() => [...document.querySelectorAll('iframe')].some((f) => f.src.includes('tosspayments.com')), { timeout: 20000 }).catch(() => {});
  // 버튼은 위젯이 다 그려진 뒤에 바뀐다(main.ts donationsPanel). iframe이 보인 뒤에도 느린 망에서는 몇 초 더 걸린다
  await d.waitForFunction(() => [...document.querySelectorAll('.donations-panel button')].some((b) => b.textContent === '결제하기' && !b.hidden), { timeout: 20000 }).catch(() => {});
  const frames = d.frames().map((f) => f.url());
  check('toss widget frames load', frames.some((u) => u.includes('payment-widget.tosspayments.com')), frames.join(' '));
  const btns = await d.$$eval('.donations-panel button', (bs) => bs.map((b) => ({ t: b.textContent, hidden: b.hidden })));
  check('start hidden, pay shown', btns.find((b) => b.t === '후원하기')?.hidden === true && btns.find((b) => b.t === '결제하기')?.hidden === false, JSON.stringify(btns));
  check('amount locked after order', await d.$eval('.donations-panel select', (s) => s.disabled));
  // 서버에 대기 주문이 생겼고 합계에는 아직 없다
  const myOrder = await d.evaluate(async (bid) => (await (await fetch(`/api/broadcasts/${bid}/donations`)).json()).total, bid);
  check('pending not in total', myOrder === 0);

  // 결제창에서 돌아온 화면: 가짜 결제 키 → 서버가 거절, 주소는 바로 방송 화면으로
  const ret = await guest();
  await ret.goto(`${BASE}/donate/success?b=${bid}&paymentKey=tgen_fake_key&orderId=onair-nothere&amount=5000`, { waitUntil: 'networkidle2' });
  await sleep(1500);
  check('return page shows failure', (await ret.$eval('main', (m) => m.innerText)).includes('확인하지 못했습니다'));
  check('return url cleaned', ret.url().endsWith(`/#/b/${bid}`) && !ret.url().includes('paymentKey'), ret.url());
  await ret.click('.topbar a');
  await sleep(1200);
  check('header link leaves return page', !(await ret.$eval('main', (m) => m.innerText)).includes('확인하는 중') && ret.url().endsWith('/#/'), ret.url());
  const fail = await ctx();
  await fail.goto(`${BASE}/donate/fail?b=${bid}&code=PAY_PROCESS_CANCELED&message=${encodeURIComponent('사용자가 결제를 취소했습니다')}`, { waitUntil: 'networkidle2' });
  await sleep(800);
  check('fail page shows reason', (await fail.$eval('main', (m) => m.innerText)).includes('사용자가 결제를 취소했습니다'));
  // 휴대폰 너비
  await anon.setViewport({ width: 375, height: 800, isMobile: true, hasTouch: true });
  await anon.reload({ waitUntil: 'networkidle2' });
  await sleep(500);
  check('fits phone width', await anon.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (bid) => fetch(`/api/broadcasts/${bid}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
