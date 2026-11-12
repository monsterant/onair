// 심사 시뮬레이션에서 나온 화면 다듬기 시험 (#55)
// node ux.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--auto-select-desktop-capture-source=Entire screen', '--autoplay-policy=no-user-gesture-required'],
});
let owner; let bid;
try {
  // 처음 온 사람: 소개와 시작 버튼, 하단 상태 문구 없음
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  await owner.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  await sleep(800);
  check('intro shown when logged out', (await owner.$eval('main', (m) => m.innerText)).includes('브라우저만으로 방송하고'));
  check('footer has no debug text', (await owner.$eval('.status', (s) => s.textContent)) === '');
  await owner.click('.intro button');
  await sleep(1500);
  check('intro start button logs in and opens new broadcast', owner.url().endsWith('#/new') && (await owner.$eval('header', (h) => h.innerText)).includes('guest_'), owner.url());
  // 빈 제목은 안내가 뜬다
  await owner.click('form button[type=submit]');
  await sleep(800);
  check('empty title shows message', (await owner.$eval('form .error', (e) => e.textContent)).length > 0);
  await owner.type('input[name=title]', 'UX 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  // 송출: 소유자 화면에는 자기 플레이어가 없고, 위쪽 표시가 곧 "방송 중"
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  let live = false;
  for (let i = 0; i < 20 && !live; i++) {
    await sleep(500);
    live = (await owner.$eval('.detail-head .badge', (b) => b.textContent)) === '방송 중';
  }
  check('owner badge turns live quickly', live);
  check('owner sees no own player while publishing', (await owner.$('#player')) === null && (await owner.$eval('main', (m) => m.innerText)).includes('지금 이 화면에서 송출하고 있습니다'));
  // 시청자: 소리 켜기 버튼으로 음소거가 풀린다
  const viewer = await (await browser.createBrowserContext()).newPage();
  await viewer.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await viewer.waitForSelector('.sound-toggle:not([hidden])', { timeout: 15000 });
  await sleep(4000);
  const before = await viewer.evaluate(() => document.querySelector('#player video')?.muted);
  await viewer.click('.sound-toggle');
  await sleep(800);
  const after = await viewer.evaluate(() => ({ muted: document.querySelector('#player video')?.muted, label: document.querySelector('.sound-toggle').textContent }));
  check('viewer starts muted, sound button unmutes', before === true && after.muted === false && after.label.includes('소리 끄기'), JSON.stringify({ before, after }));
  // 휴대폰 너비: 위쪽 메뉴가 한 줄
  const phone = await (await browser.createBrowserContext()).newPage();
  await phone.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
  await phone.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await phone.click('.demo .row button');
  await sleep(1500);
  const h = await phone.$eval('.topbar', (t) => t.getBoundingClientRect().height);
  const wraps = await phone.$$eval('.topbar a.button, .topbar button', (bs) => bs.map((b) => b.getBoundingClientRect().height));
  check('phone header buttons do not wrap', wraps.every((x) => x < 50), JSON.stringify({ h, wraps }));
  check('phone fits width', await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
