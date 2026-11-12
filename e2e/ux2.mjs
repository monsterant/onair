// 심사 시뮬레이션 2차 화면 다듬기 시험 (#58)
// node ux2.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const clickText = (p, sel, text) => p.evaluate((sel, text) => [...document.querySelectorAll(sel)].find((b) => b.textContent.trim() === text)?.click(), sel, text);
let owner; let bid;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  // 없는 방송: 목록으로 가는 링크
  await owner.goto(`${BASE}/#/b/99999999`, { waitUntil: 'networkidle2' });
  await sleep(800);
  check('unknown broadcast has link to list', await owner.$$eval('main a', (as) => as.some((a) => a.textContent === '방송 목록으로' && a.getAttribute('href') === '#/')));
  // 긴 설명으로 방송 만들기
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '아주 긴 제목 '.repeat(14).slice(0, 100));
  await owner.$eval('textarea[name=description]', (t) => { t.value = '긴 설명입니다. '.repeat(60); });
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  await owner.waitForSelector('.description');
  await sleep(500);
  const fold = await owner.evaluate(() => {
    const d = document.querySelector('.description'); const m = document.querySelector('button.more');
    return { folded: d.classList.contains('folded'), h: d.getBoundingClientRect().height, more: m && !m.hidden };
  });
  check('long description is folded with 더보기', fold.folded && fold.more && fold.h < 150, JSON.stringify(fold));
  await owner.click('button.more');
  await sleep(300);
  const open = await owner.evaluate(() => ({ folded: document.querySelector('.description').classList.contains('folded'), label: document.querySelector('button.more').textContent }));
  check('더보기 unfolds and becomes 접기', !open.folded && open.label === '접기', JSON.stringify(open));
  check('studio comes before description', await owner.evaluate(() => {
    const s = document.querySelector('.start-screen'); const d = document.querySelector('.description');
    return !!s && !!(s.compareDocumentPosition(d) & Node.DOCUMENT_POSITION_FOLLOWING);
  }));
  check('owner inputs have labels', await owner.evaluate(() => !!document.querySelector('input[aria-label="방송 제목"]') && !!document.querySelector('textarea[aria-label="방송 설명"]')));
  check('page fits width with long title', await owner.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await owner.$eval('input[aria-label="방송 제목"]', (i) => { i.value = '바꾼 제목'; });
  await clickText(owner, 'button', '저장');
  await sleep(1200);
  check('save shows 저장했습니다', (await owner.$eval('main', (m) => m.innerText)).includes('저장했습니다.'));
  await clickText(owner, 'button', '송출 주소 보기');
  await sleep(1200);
  check('WHIP address has copy button', await owner.evaluate(() => [...document.querySelectorAll('.publish button')].some((b) => b.textContent === '주소 복사')));
  // 로그아웃 상태로 방송을 보다 로그인하면 그 방송으로 돌아온다
  const v = await (await browser.createBrowserContext()).newPage();
  await v.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await sleep(800);
  await v.click('.topbar a[href="#/login"]');
  await sleep(800);
  await v.click('.demo .row button');
  await sleep(1500);
  check('login returns to the broadcast', v.url().endsWith(`#/b/${bid}`), v.url());
  // 휴대폰: 댓글 버튼이 꽉 차지 않는다
  await v.type('.comment-form textarea', '휴대폰 시험 댓글');
  await v.click('.comment-form button');
  await sleep(1200);
  await v.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
  await sleep(800);
  const w = await v.evaluate(() => [...document.querySelectorAll('.comment .row > button')].map((b) => b.getBoundingClientRect().width));
  check('phone comment buttons not full width', w.length > 0 && w.every((x) => x < 200), JSON.stringify(w));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
