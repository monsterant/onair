// 시청자 인터넷 끊김 알림 시험 (#63)
// node offline.mjs [BASE]
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
const noteShown = (p) => p.evaluate(() => { const n = document.querySelector('.net-note'); return !!n && !n.hidden; });
const videoTime = (p) => p.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(300); }
  return false;
}
async function playing(p, ms) {
  return waitFor(async () => { const a = await videoTime(p); await sleep(700); const b = await videoTime(p); return a >= 0 && b > a; }, ms);
}
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
  await owner.type('input[name=title]', '끊김 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  await waitFor(async () => (await owner.$eval('.detail-head .badge', (b) => b.textContent)) === '방송 중', 10000);

  const viewer = await (await browser.createBrowserContext()).newPage();
  await viewer.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('viewer plays', await playing(viewer, 20000));
  check('no notice while connected', !(await noteShown(viewer)));

  // 1) 인터넷이 끊김(브라우저가 offline을 안다) → 바로 알림
  await viewer.setOfflineMode(true);
  check('offline shows notice quickly', await waitFor(() => noteShown(viewer), 3000));
  await sleep(6000);
  check('notice stays while offline', await noteShown(viewer));
  await viewer.setOfflineMode(false);
  check('online hides notice', await waitFor(async () => !(await noteShown(viewer)), 4000));
  check('video plays again after reconnect', await playing(viewer, 15000));

  // 2) 브라우저는 온라인이라 하지만 서버에 닿지 못함 → 두 번 실패(약 10초) 뒤 알림, 한 번은 알리지 않음
  await viewer.setRequestInterception(true);
  let block = true;
  viewer.on('request', (r) => (block && r.url().includes('/api/') ? r.abort() : r.continue()));
  // 상태 확인은 5초마다라 4초 안에는 많아야 1번 실패한다
  await sleep(4000);
  const early = await noteShown(viewer);
  const later = await waitFor(() => noteShown(viewer), 11000);
  check('server unreachable: notice after repeated failures, not after one', !early && later, JSON.stringify({ early, later }));
  block = false;
  check('server reachable again hides notice', await waitFor(async () => !(await noteShown(viewer)), 8000));
  check('video still plays', await playing(viewer, 15000));
  await viewer.setRequestInterception(false);
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
