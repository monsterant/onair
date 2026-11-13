// 실시간 채팅 시험 (#86): 방송자·다른 사용자·비로그인 세 창에서 주고받기, 비로그인은 읽기만, 종료 방송은 쓰기 막힘
// node chat.mjs [BASE]
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
const lines = (p) => p.$$eval('.chat-list .chat-line', (ls) => ls.map((l) => l.textContent)).catch(() => []);
const has = async (p, t) => (await lines(p)).some((l) => l.includes(t));
async function guest() {
  const p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
}
async function say(p, text) {
  await p.type('.chat-form input', text);
  await p.click('.chat-form button');
}

let owner; let bid;
try {
  owner = await guest();
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '채팅 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  await owner.waitForSelector('.chat-panel');
  check('chat panel shows empty state', (await owner.$eval('.chat-list', (e) => e.textContent)).includes('아직 채팅이 없습니다'));

  const other = await guest();
  await other.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  const anon = await (await browser.createBrowserContext()).newPage();
  await anon.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await other.waitForSelector('.chat-form input');
  check('anonymous has no chat input', !(await anon.$('.chat-form')));
  check('anonymous sees login hint', (await anon.$eval('.chat-panel', (e) => e.textContent)).includes('로그인하면 채팅할 수 있습니다'));
  // 휴대폰 너비에서 입력 칸이 버튼보다 넓다(#90 R6)
  await other.setViewport({ width: 375, height: 740, isMobile: true, hasTouch: true });
  await sleep(300);
  const w = await other.evaluate(() => [document.querySelector('.chat-form input').getBoundingClientRect().width, document.querySelector('.chat-form button').getBoundingClientRect().width, document.documentElement.scrollWidth <= innerWidth + 1]);
  check('phone: input wider than send button, no horizontal scroll', w[0] > w[1] && w[2], JSON.stringify(w));
  await other.setViewport({ width: 1280, height: 800 });
  await sleep(1000);

  // 방송자가 쓴 글이 다른 두 창에 바로 온다
  const t0 = Date.now();
  await say(owner, '안녕하세요 방송자입니다');
  const otherGot = await waitFor(() => has(other, '안녕하세요 방송자입니다'), 5000);
  const ms = Date.now() - t0;
  check('other user receives owner message quickly', otherGot && ms < 3000, `${ms}ms`);
  check('anonymous receives owner message', await waitFor(() => has(anon, '안녕하세요 방송자입니다'), 5000));
  check('input cleared after send', (await owner.$eval('.chat-form input', (i) => i.value)) === '');
  // 다른 사용자가 쓰면 방송자에게 온다. 이름은 서버가 붙인다.
  await say(other, '반가워요');
  check('owner receives other user message', await waitFor(() => has(owner, '반가워요'), 5000));
  const ownerLines = await lines(owner);
  check('own message appears once (no duplicates)', ownerLines.filter((l) => l.includes('안녕하세요 방송자입니다')).length === 1, JSON.stringify(ownerLines));
  check('username shown', /^guest_\w+ 반가워요$/.test(ownerLines.find((l) => l.includes('반가워요')) ?? ''), JSON.stringify(ownerLines));
  // 늦게 들어온 사람도 최근 대화를 본다
  const late = await (await browser.createBrowserContext()).newPage();
  await late.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('late viewer sees recent messages', await waitFor(async () => (await has(late, '안녕하세요 방송자입니다')) && (await has(late, '반가워요')), 5000));
  // 스크립트로 쓴 글이 실행되지 않는다(글로만 보임)
  await say(other, '<img src=x onerror="window.__xss=1">');
  await waitFor(() => has(owner, '<img src=x'), 5000);
  check('message is text, not HTML', !(await owner.evaluate(() => window.__xss)) && !(await owner.$('.chat-list img')));
  // 방송이 끝나면 쓸 수 없다
  for (const b of await owner.$$('.owner button')) if ((await b.evaluate((x) => x.textContent)) === '방송 종료') await b.click();
  await sleep(2500);
  await say(other, '끝났나요');
  await sleep(1500);
  check('cannot chat on ended broadcast', !(await has(owner, '끝났나요')) && (await other.$eval('.chat-panel .error', (e) => e.textContent)).includes('종료된 방송'), await other.$eval('.chat-panel .error', (e) => e.textContent).catch(() => ''));

  // 종료된 방송을 새로 열면 입력 칸 대신 안내(#90 R5)
  await other.reload({ waitUntil: 'networkidle2' });
  await other.waitForSelector('.chat-panel');
  check('ended broadcast shows notice instead of input', !(await other.$('.chat-form')) && (await other.$eval('.chat-panel', (e) => e.textContent)).includes('종료된 방송이라'));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
