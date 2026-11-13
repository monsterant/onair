// 지금 보는 사람 수 시험 (#85): 시청자가 늘고 줄면 방송 화면과 목록의 "N명 시청 중"이 따라 바뀐다
// node viewers.mjs [BASE]
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
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(500); }
  return false;
}
const viewersText = (p) => p.$eval('.detail-head', (e) => e.querySelector('.viewers')?.textContent ?? '').catch(() => '');

let owner; let bid;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '시청자 수 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  check('no viewer count before live', (await viewersText(owner)) === '');
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  await waitFor(async () => (await owner.$eval('.detail-head .badge', (b) => b.textContent)) === '방송 중', 10000);

  // 시청자 두 명(다른 브라우저 창)
  const v1 = await (await browser.createBrowserContext()).newPage();
  await v1.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  const v2 = await (await browser.createBrowserContext()).newPage();
  await v2.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('viewer sees "2명 시청 중"', await waitFor(async () => (await viewersText(v1)) === '2명 시청 중', 25000), await viewersText(v1));
  // 목록에도 나온다
  const list = await (await browser.createBrowserContext()).newPage();
  await list.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  const card = await list.$$eval('.card', (cs, id) => cs.find((c) => c.getAttribute('href') === `#/b/${id}`)?.textContent ?? '', bid);
  check('list card shows viewer count', /\d+명 시청 중/.test(card), card);
  await list.close();

  // 한 명이 나가면 줄어든다
  await v2.close();
  check('count drops to 1 after a viewer leaves', await waitFor(async () => (await viewersText(v1)) === '1명 시청 중', 30000), await viewersText(v1));

  // 방송이 끝나면 사라진다
  await owner.click('.stop-publish');
  check('count hidden when not live', await waitFor(async () => (await viewersText(v1)) === '', 25000), await viewersText(v1));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
