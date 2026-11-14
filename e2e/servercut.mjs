// 서버가 송출을 끊었을 때 방송자 화면이 바로 아는지 시험 (#136).
// 주인 창에서 화면 공유 송출 → 다른 창(같은 계정)에서 송출 키를 다시 만듦(서버가 옛 송출을 끊음)
// → 주인 창이 브라우저의 "연결 실패" 판단(약 9초)이나 15초 기다림 없이 "서버에서 송출이 끊겼습니다"를 보여 준다.
// node servercut.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--auto-select-desktop-capture-source=Entire screen'],
});
const pubState = (p) => p.$eval('.pub-state', (e) => e.textContent).catch(() => null);

let owner; let id;
try {
  const ctx = await browser.createBrowserContext();
  owner = await ctx.newPage();
  owner.on('dialog', (d) => d.accept());
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  id = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '서버 끊김 시험' }) })).json())).id;
  await owner.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  let live = false;
  for (let i = 0; i < 30 && !live; i++) { live = (await pubState(owner)) === '송출 중'; if (!live) await sleep(500); }
  check('publishing live', live, String(await pubState(owner)));
  await sleep(3000);
  // 다른 창(같은 계정 쿠키)에서 송출 키를 다시 만든다 → 서버가 옛 송출을 끊는다
  const other = await ctx.newPage();
  await other.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  const t0 = Date.now();
  const rotated = await other.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/stream-key`, { method: 'POST' })).status, id);
  check('key rotated by server', rotated === 200, `status=${rotated}`);
  let msg = '';
  while (Date.now() - t0 < 20000) {
    msg = await owner.$eval('.studio', (e) => e.innerText).catch(() => '');
    if (/송출을 멈췄습니다|송출이 끊겼습니다/.test(msg)) break;
    await sleep(250);
  }
  const secs = ((Date.now() - t0) / 1000).toFixed(1);
  results.push(`INFO owner noticed after ${secs}s`);
  check('owner told the server cut the stream', msg.includes('서버에서 송출이 끊겼습니다'), msg.replace(/\s+/g, ' ').slice(0, 200));
  check('noticed within 8s', Number(secs) <= 8, `${secs}s`);
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && id) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), id).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
