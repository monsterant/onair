// 댓글 화면 시험 (#37): 작성자 A, 다른 사용자 B(시크릿 창), 비로그인 C.
// node comments.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);

const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
let bid;
let aPage;
try {
  const ctx = async () => (await browser.createBrowserContext()).newPage();
  const guestPage = async () => {
    const p = await ctx();
    let dialogs = 0;
    p.on('dialog', (d) => { dialogs++; d.accept(); });
    p.dialogs = () => dialogs;
    await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
    await p.click('.demo .row button');
    await sleep(1000);
    return p;
  };
  const items = (p) => p.$$eval('.comments .comment', (ls) => ls.map((l) => ({
    author: l.querySelector('strong')?.textContent,
    body: l.querySelector('.comment-body')?.textContent,
    buttons: [...l.querySelectorAll('button')].map((b) => b.textContent),
    edited: l.textContent.includes('(수정됨)'),
  })));

  const a = await guestPage();
  aPage = a;
  bid = await a.evaluate(async () => (await (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '댓글 시험' }) })).json()).id);
  await a.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await a.waitForSelector('.comments-panel textarea');
  // 첫 목록 응답을 기다린다(배포 서버는 시드니라 응답이 늦다)
  await a.waitForFunction(() => /\d+개/.test(document.querySelector('.comments-panel h3')?.textContent ?? ''), { timeout: 10000 }).catch(() => {});
  check('empty list shows 0', (await a.$eval('.comments-panel h3', (h) => h.textContent)).includes('0개'));

  // A가 쓴다 (스크립트처럼 보이는 글도 글자 그대로 보여야 한다)
  const xss = '<img src=x onerror="window.__x=1">안녕하세요';
  await a.type('.comment-form textarea', xss);
  await a.click('.comment-form button[type=submit]');
  await sleep(800);
  let list = await items(a);
  check('author sees own comment', list.length === 1 && list[0].body === xss, JSON.stringify(list));
  check('author has edit/delete', list[0]?.buttons.join() === '수정,삭제', JSON.stringify(list[0]));
  check('no script ran', !(await a.evaluate(() => window.__x)) && (await a.$$('.comments img')).length === 0);
  check('input cleared after send', (await a.$eval('.comment-form textarea', (t) => t.value)) === '');

  // 빈 댓글은 서버가 거절하고 안내가 뜬다
  await a.type('.comment-form textarea', '   ');
  await a.click('.comment-form button[type=submit]');
  await sleep(600);
  check('empty comment shows error', (await a.$eval('.comment-form .error', (e) => e.textContent)).length > 0);
  await a.$eval('.comment-form textarea', (t) => { t.value = ''; });

  // A가 고친다
  await a.click('.comments .comment button');
  await a.waitForSelector('.comments .comment textarea');
  await a.$eval('.comments .comment textarea', (t) => { t.value = ''; });
  await a.type('.comments .comment textarea', '고친');
  // 목록 새로고침(4초)이 지나도 수정 칸의 커서가 그대로이고, 이어서 친 글자가 들어가야 한다(#37 리뷰)
  await sleep(4600);
  check('edit box keeps focus across poll', await a.evaluate(() => document.activeElement?.getAttribute('aria-label') === '댓글 수정'));
  await a.keyboard.type(' 댓글');
  check('typing after poll goes into edit box', (await a.$eval('.comments .comment textarea', (t) => t.value)) === '고친 댓글');
  const saveBtn = await a.$$('.comments .comment button');
  await saveBtn[0].click();
  await sleep(800);
  list = await items(a);
  check('edit saved and marked', list[0]?.body === '고친 댓글' && list[0]?.edited, JSON.stringify(list));

  // B(다른 사용자): A의 댓글은 보이지만 버튼이 없다. 자기 댓글을 쓴다
  const b = await guestPage();
  await b.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await b.waitForSelector('.comments .comment');
  list = await items(b);
  check('other sees comment without buttons', list.length === 1 && list[0].buttons.length === 0, JSON.stringify(list));
  const cid = await b.evaluate(async (bid) => (await (await fetch(`/api/broadcasts/${bid}/comments`)).json())[0].id, bid);
  const forged = await b.evaluate(async (cid) => [
    (await fetch(`/api/comments/${cid}`, { method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ body: '남이 고침' }) })).status,
    (await fetch(`/api/comments/${cid}`, { method: 'DELETE' })).status,
  ], cid);
  check('other gets 403 via API', forged.join() === '403,403', forged.join());
  await b.type('.comment-form textarea', 'B의 댓글');
  await b.click('.comment-form button[type=submit]');
  await sleep(800);

  // A 화면에 B의 댓글이 몇 초 안에 나타난다(폴링)
  await sleep(4500);
  list = await items(a);
  check('author sees new comment by polling', list.length === 2 && list[1].body === 'B의 댓글' && list[1].buttons.length === 0, JSON.stringify(list));

  // C(비로그인): 목록은 보고, 입력 칸 대신 로그인 안내
  const c = await ctx();
  await c.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await c.waitForSelector('.comments .comment');
  check('anonymous sees comments', (await items(c)).length === 2);
  check('anonymous has no form', (await c.$('.comment-form')) === null && (await c.$eval('.comments-panel', (p) => p.textContent)).includes('로그인'));

  // A가 지운다(확인 창)
  await a.click('.comments .comment button.danger');
  await sleep(800);
  list = await items(a);
  check('delete after confirm', a.dialogs() === 1 && list.length === 1 && list[0].body === 'B의 댓글', JSON.stringify(list));

  // A가 자기 댓글을 고치는 동안, 그 위의 B 댓글이 지워져도 수정 칸 커서가 그대로여야 한다(#37 2바퀴)
  await a.type('.comment-form textarea', 'A의 아래 댓글');
  await a.click('.comment-form button[type=submit]');
  await sleep(800);
  const editBtns = await a.$$('.comments .comment:last-child button');
  await editBtns[0].click();
  await a.waitForSelector('.comments .comment:last-child textarea');
  await a.type('.comments .comment:last-child textarea', ' 수정중');
  const bCid = await b.evaluate(async (bid) => (await (await fetch(`/api/broadcasts/${bid}/comments`)).json()).find((x) => x.body === 'B의 댓글').id, bid);
  await b.evaluate(async (id) => fetch(`/api/comments/${id}`, { method: 'DELETE' }), bCid);
  await sleep(4600);
  check('focus kept when a comment above is deleted', await a.evaluate(() => document.activeElement?.getAttribute('aria-label') === '댓글 수정'));
  await a.keyboard.type('!');
  check('typing still goes into edit box', (await a.$eval('.comments .comment textarea', (t) => t.value)).endsWith('수정중!'));
  check('deleted comment above is gone', (await items(a)).length === 1);

  // 휴대폰 너비에서 가로 스크롤 없음
  await c.setViewport({ width: 375, height: 800, isMobile: true, hasTouch: true });
  await c.reload({ waitUntil: 'networkidle2' });
  await sleep(500);
  check('fits phone width', await c.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (aPage && bid) await aPage.evaluate(async (bid) => fetch(`/api/broadcasts/${bid}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
