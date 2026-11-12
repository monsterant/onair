// 녹화·다시보기 시험 (#47): 실제로 화면 공유로 송출 → 종료 → 녹화가 다시보기에 올라오고 재생·탐색된다.
// node recording.mjs [BASE]
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
let owner; let bid;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.click('.demo .row button');
  await sleep(1000);
  bid = await owner.evaluate(async () => (await (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '녹화 시험 방송' }) })).json()).id);
  await owner.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  await owner.click('.start-screen');
  // live가 될 때까지
  let live = false;
  for (let i = 0; i < 30 && !live; i++) {
    await sleep(1000);
    live = (await owner.evaluate(async (id) => (await (await fetch(`/api/broadcasts/${id}`)).json()).status, bid)) === 'live';
  }
  check('broadcast goes live', live);
  // 녹화가 시작되려면 맞춰 보기(15초) 한 번이 지나야 한다. 25초 송출한다.
  await sleep(25000);
  for (const b of await owner.$$('.owner button')) if ((await b.evaluate((x) => x.textContent)) === '방송 종료') { owner.once('dialog', (d) => d.accept()); await b.click(); }
  await sleep(1500);
  const st = await owner.evaluate(async (id) => (await (await fetch(`/api/broadcasts/${id}`)).json()).status, bid);
  check('broadcast ended', st === 'ended', st);
  // 다시보기가 올라올 때까지(최대 75초)
  let recs = [];
  for (let i = 0; i < 25 && recs.length === 0; i++) {
    await sleep(3000);
    recs = await owner.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/recordings`)).json(), bid);
  }
  check('recording appears after end', recs.length >= 1, JSON.stringify(recs));
  if (recs.length) {
    const r = recs[0];
    check('recording keeps broadcast info', r.title === '녹화 시험 방송' && r.format === 'mp4' && r.bytes > 10000, JSON.stringify(r));
    // 비로그인 시청자가 다시보기 목록과 상세에서 재생
    const viewer = await (await browser.createBrowserContext()).newPage();
    await viewer.goto(`${BASE}/#/recordings`, { waitUntil: 'networkidle2' });
    await sleep(800);
    check('listed on replay page', (await viewer.$$eval('.list .card', (cs) => cs.map((c) => c.getAttribute('href')))).includes(`#/r/${r.id}`));
    await viewer.goto(`${BASE}/#/r/${r.id}`, { waitUntil: 'networkidle2' });
    await viewer.waitForSelector('video.replay');
    const info = await viewer.evaluate(async () => {
      const v = document.querySelector('video.replay');
      v.muted = true;
      await new Promise((ok) => (v.readyState >= 1 ? ok() : v.addEventListener('loadedmetadata', ok, { once: true })));
      await v.play().catch(() => {});
      await new Promise((ok) => setTimeout(ok, 2000));
      const t1 = v.currentTime;
      v.currentTime = Math.max(0, v.duration - 3);
      await new Promise((ok) => setTimeout(ok, 1500));
      return { duration: v.duration, w: v.videoWidth, h: v.videoHeight, t1, t2: v.currentTime, err: v.error && v.error.code };
    });
    check('replay plays with video', info.duration > 8 && info.w > 0 && info.t1 > 0.5 && !info.err, JSON.stringify(info));
    check('replay seeks near the end', info.t2 > info.duration - 5, JSON.stringify(info));
    const headers = await viewer.evaluate(async (u) => { const res = await fetch(`/api${u}`, { headers: { Range: 'bytes=0-99' } }); return [res.status, res.headers.get('content-range')]; }, r.url);
    check('range request supported', headers[0] === 206 && /^bytes 0-99\//.test(headers[1] ?? ''), JSON.stringify(headers));
    // 방송 상세에도 이 방송의 다시보기 칸이 보인다
    await viewer.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
    await sleep(1000);
    check('broadcast page shows its replay', await viewer.$eval('.recordings-panel', (p) => !p.hidden && p.innerText.includes('녹화 시험 방송')));
  }
} catch (e) {
  check('no exception', false, e.stack);
}
console.log(results.join('\n'));
console.log('BID', bid);
await browser.close();
