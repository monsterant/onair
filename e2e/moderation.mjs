// 시청자 제한·천천히 모드 시험 (#87): 방송 주인이 채팅 옆 버튼으로 막고 풀기, 막힌 사람은 채팅·댓글 못 씀, 천천히 모드
// node moderation.mjs [BASE]
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
const text = (p, sel) => p.$eval(sel, (e) => e.textContent ?? '').catch(() => '');
const has = async (p, t) => (await p.$$eval('.chat-list .chat-line', (ls) => ls.map((l) => l.textContent)).catch(() => [])).some((l) => l.includes(t));
async function guest() {
  const p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
}
async function say(p, t) {
  await p.$eval('.chat-form input', (i) => (i.value = ''));
  await p.type('.chat-form input', t);
  await p.click('.chat-form button');
}

let owner; let bid;
try {
  owner = await guest();
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '제한 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  await owner.waitForSelector('.chat-panel .slow-select');
  check('owner sees slow mode select', true);

  const other = await guest();
  await other.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await other.waitForSelector('.chat-form input');
  check('viewer has no owner tools', !(await other.$('.slow-select')) && !(await other.$('.ban-buttons')));
  await sleep(800);
  await say(other, '안녕하세요');
  check('owner sees ban buttons next to viewer chat', await waitFor(async () => !!(await owner.$('.chat-line .ban-buttons')), 5000));
  // 주인 자신의 채팅에는 버튼이 없다
  await say(owner, '주인 글');
  await waitFor(() => has(owner, '주인 글'), 5000);
  const ownLine = await owner.$$eval('.chat-line', (ls) => ls.find((l) => l.textContent.includes('주인 글'))?.querySelector('.ban-buttons') ? 'has' : 'none');
  check('no ban buttons on own chat', ownLine === 'none');

  // 10분 제한
  const btn = await owner.$$('.chat-line .ban-buttons button');
  await btn[0].click();
  check('owner sees ban confirmation', await waitFor(async () => (await text(owner, '.chat-panel .error')).includes('막았습니다'), 5000), await text(owner, '.chat-panel .error'));
  check('ban list shows user', await waitFor(async () => (await text(owner, '.ban-list')).includes('까지'), 5000), await text(owner, '.ban-list'));
  await say(other, '막힌 뒤');
  check('banned viewer cannot chat', await waitFor(async () => (await text(other, '.chat-panel .error')).includes('막았습니다'), 5000), await text(other, '.chat-panel .error'));
  check('banned message not shown', !(await has(owner, '막힌 뒤')));
  await other.type('.comment-form textarea', '막힌 뒤 댓글');
  await other.click('.comment-form button[type=submit]');
  check('banned viewer cannot comment', await waitFor(async () => (await text(other, '.comments-panel .error')).includes('막았습니다'), 5000), await text(other, '.comments-panel .error'));
  // 해제
  await owner.click('.ban-list button');
  await waitFor(async () => (await text(owner, '.ban-list')) === '', 5000);
  // 댓글 옆 버튼으로 막아도 채팅 칸 목록에 바로 보인다(#94 R3). 확인 뒤 다시 푼다.
  await other.type('.comment-form textarea', '댓글 하나');
  await other.click('.comment-form button[type=submit]');
  await waitFor(async () => (await text(other, '.comments-panel')).includes('댓글 하나'), 5000);
  await owner.reload({ waitUntil: 'networkidle2' });
  await owner.waitForSelector('.comments-panel .ban-buttons button');
  await (await owner.$$('.comments-panel .ban-buttons button'))[0].click();
  check('comment ban shows in chat ban list within 3s', await waitFor(async () => (await text(owner, '.ban-list')).includes('까지'), 3000), await text(owner, '.ban-list'));
  await owner.click('.ban-list button');
  await waitFor(async () => (await text(owner, '.ban-list')) === '', 5000);
  await say(other, '풀린 뒤');
  check('unbanned viewer can chat again', await waitFor(() => has(owner, '풀린 뒤'), 5000));

  // 천천히 모드 10초
  await owner.select('.chat-panel .slow-select', '10');
  check('owner sees slow mode set', await waitFor(async () => (await text(owner, '.chat-panel .error')).includes('10초'), 5000), await text(owner, '.chat-panel .error'));
  check('viewer sees slow mode hint', await waitFor(async () => (await text(other, '.slow-hint')).includes('10초에 한 번'), 12000), await text(other, '.slow-hint'));
  await say(other, '천천히 하나');
  await waitFor(() => has(owner, '천천히 하나'), 5000);
  await say(other, '천천히 둘');
  check('second message within slow mode is rejected', await waitFor(async () => (await text(other, '.chat-panel .error')).includes('천천히 모드'), 5000), await text(other, '.chat-panel .error'));
  await say(owner, '주인은 예외 1');
  await sleep(300);
  await say(owner, '주인은 예외 2');
  check('owner is not slowed', await waitFor(() => has(other, '주인은 예외 2'), 5000));
  // 공지 고정(#109): 주인이 올리면 시청자에게 보인다. 시청자에게는 입력 칸이 없다
  check('viewer has no notice input', !(await other.$('.notice-input')));
  await owner.type('.notice-input', '오늘은 9시까지');
  for (const b of await owner.$$('.notice-tools button')) if ((await b.evaluate((x) => x.textContent)) === '공지 올리기') await b.click();
  check('owner sees notice', await waitFor(async () => (await text(owner, '.chat-notice')).includes('오늘은 9시까지'), 5000), await text(owner, '.chat-notice'));
  check('viewer sees notice', await waitFor(async () => (await text(other, '.chat-notice')).includes('오늘은 9시까지'), 12000), await text(other, '.chat-notice'));
  // 비우고 올리면 내려간다
  await owner.$eval('.notice-input', (i) => (i.value = ''));
  for (const b of await owner.$$('.notice-tools button')) if ((await b.evaluate((x) => x.textContent)) === '공지 올리기') await b.click();
  check('notice removed for viewer', await waitFor(async () => other.$eval('.chat-notice', (e) => e.hidden), 12000));
  // 채팅 금칙어(#112): 주인이 저장하면 그 단어가 든 채팅은 거절된다. 시청자에게는 입력 칸이 없다
  check('viewer has no banned words input', !(await other.$('.words-input')));
  await owner.select('.chat-panel .slow-select', '0');
  await owner.type('.words-input', ' Spam , 광고 ');
  for (const b of await owner.$$('.words-tools button')) if ((await b.evaluate((x) => x.textContent)) === '금칙어 저장') await b.click();
  check('owner sees cleaned words', await waitFor(async () => (await owner.$eval('.words-input', (i) => i.value)) === 'spam, 광고', 5000), await owner.$eval('.words-input', (i) => i.value));
  await say(other, '무료 SPAM 사이트');
  check('banned word rejected', await waitFor(async () => (await text(other, '.chat-panel .error')).includes('막은 단어'), 5000), await text(other, '.chat-panel .error'));
  check('banned word message not shown', !(await has(owner, '무료 SPAM')));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
