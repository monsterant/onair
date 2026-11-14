// 방송 기록 요약 시험 (#110): 송출 → 시청자가 보고 채팅 → 송출을 멈췄다 다시 시작 → 종료하면
// 주인 칸에 "방송 시간 · 최고 시청자 · 채팅 · 후원" 기록이 뜬다. 방송 시간은 처음 시작부터라 두 번째 송출 구간보다 길다.
// 다른 사람(시청자)은 기록 API가 403이다.
// node summary.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required', '--auto-select-desktop-capture-source=Entire screen'],
});
const pubState = (p) => p.$eval('.pub-state', (e) => e.textContent).catch(() => null);
async function waitFor(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(300); }
  return false;
}
const guest = async () => {
  const p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
};

let owner; let id;
try {
  owner = await guest();
  id = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '기록 요약 시험' }) })).json())).id;
  await owner.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  check('publishing live', await waitFor(async () => (await pubState(owner)) === '송출 중'), String(await pubState(owner)));

  // 시청자가 들어와 WebRTC로 본다. 상세 화면이 5초마다 다시 불러올 때 시청자 수가 기록된다.
  const viewer = await guest();
  await viewer.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  const counted = await waitFor(async () => (await viewer.evaluate(async (id) => (await (await fetch(`/api/broadcasts/${id}`)).json()).viewers, id)) >= 1, 30000);
  check('viewer counted', counted);
  await viewer.waitForSelector('.chat-form input');
  await viewer.type('.chat-form input', '잘 보여요');
  await viewer.click('.chat-form button');
  await sleep(1000);
  const forbidden = await viewer.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/summary`)).status, id);
  check('other user cannot see summary', forbidden === 403, `status=${forbidden}`);

  // 송출을 멈췄다가 다시 시작한다(두 번째 구간)
  await owner.click('.stop-publish');
  await waitFor(async () => (await pubState(owner)) !== '송출 중', 10000);
  await sleep(4000);
  await owner.click('.start-screen');
  const second = Date.now();
  check('publishing live again', await waitFor(async () => (await pubState(owner)) === '송출 중'), String(await pubState(owner)));
  await sleep(3000);
  const ended = await owner.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/end`, { method: 'POST' })).status, id);
  const secondSegment = Math.round((Date.now() - second) / 1000);
  check('ended', ended === 200, `status=${ended}`);
  const shown = await waitFor(async () => (await owner.$eval('.broadcast-summary', (e) => (e.hidden ? '' : e.textContent)).catch(() => '')).includes('방송 기록'), 15000);
  const text = await owner.$eval('.broadcast-summary', (e) => e.textContent).catch(() => '');
  results.push(`INFO ${text} (두 번째 구간 약 ${secondSegment}초)`);
  check('summary shown to owner', shown, text);
  check('summary counts the chat', text.includes('채팅 1개'), text);
  check('summary has peak viewers >= 1', /최고 시청자 [1-9]\d*명/.test(text), text);
  const s = await owner.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/summary`)).json(), id);
  check('duration counts from the first start', s.duration_secs > secondSegment + 3, `duration=${s.duration_secs} second=${secondSegment}`);
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && id) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), id).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
