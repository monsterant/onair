// 다시보기 장면 저장 시험 (#111): 다시보기에서 구간을 장면으로 저장 → 목록에 뜸 → 장면 링크는 그 구간만 재생하고 끝에서 멈춤
// → 다른 사람에게는 지우기 버튼이 없고, 만든 사람은 지운다. 다시보기가 하나도 없으면 건너뛴다(먼저 recording.mjs를 돌린다).
// node clips.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--autoplay-policy=no-user-gesture-required', '--mute-audio'] });
async function waitFor(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(250); }
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

let a; let clipId;
try {
  a = await guest();
  // 3초 넘는 다시보기를 고른다
  const rid = await a.evaluate(async () => {
    const list = await (await fetch('/api/recordings')).json();
    const long = list.find((r) => r.ended_at && new Date(r.ended_at) - new Date(r.started_at) > 5000);
    return long?.id;
  });
  if (!rid) {
    results.push('INFO 5초 넘는 다시보기가 없어 건너뜀(먼저 recording.mjs를 돌린다)');
  } else {
    await a.goto(`${BASE}/#/r/${rid}`, { waitUntil: 'networkidle2' });
    await a.waitForSelector('.clip-form');
    await a.type('.clip-form input[type=text]', '시험 장면');
    await a.$eval('.clip-start', (e) => (e.value = '1'));
    await a.$eval('.clip-end', (e) => (e.value = '3'));
    for (const b of await a.$$('.clip-form button')) if ((await b.evaluate((x) => x.textContent)) === '장면 저장') await b.click();
    const listed = await waitFor(async () => (await a.$$eval('.clip-list a', (ls) => ls.map((l) => l.textContent))).includes('시험 장면'));
    check('saved clip is listed', listed);
    clipId = await a.$$eval('.clip-list a', (ls) => ls.find((l) => l.textContent === '시험 장면')?.getAttribute('href')?.split('/').pop());
    check('maker sees delete button', (await a.$$('.clip-list button')).length > 0);

    // 장면 화면: 1초에서 시작하고 3초에서 멈춘다
    await a.goto(`${BASE}/#/c/${clipId}`, { waitUntil: 'networkidle2' });
    await a.waitForSelector('.clip-video');
    await waitFor(() => a.$eval('.clip-video', (v) => v.readyState >= 1));
    const t0 = await a.$eval('.clip-video', (v) => v.currentTime);
    check('clip starts at its start', t0 >= 0.9 && t0 <= 1.5, `t=${t0}`);
    await a.$eval('.clip-video', (v) => v.play());
    const stopped = await waitFor(async () => a.$eval('.clip-video', (v) => v.paused && v.currentTime >= 2.9), 10000);
    const t1 = await a.$eval('.clip-video', (v) => v.currentTime);
    check('clip stops at its end', stopped && t1 <= 3.5, `t=${t1}`);

    // 다른 사람: 장면은 보이지만 지우기 버튼이 없고, API로 지우면 403
    const b = await guest();
    await b.goto(`${BASE}/#/r/${rid}`, { waitUntil: 'networkidle2' });
    await waitFor(async () => (await b.$$eval('.clip-list a', (ls) => ls.length)) > 0);
    check('other user sees the clip', (await b.$$eval('.clip-list a', (ls) => ls.map((l) => l.textContent))).includes('시험 장면'));
    // 이 장면 줄에 지우기 버튼이 없어야 한다
    const rowHasButton = await b.$$eval('.clip-list li', (ls, id) => ls.some((l) => l.querySelector(`a[href="#/c/${id}"]`) && l.querySelector('button')), clipId);
    check('other user has no delete button', !rowHasButton);
    const st = await b.evaluate(async (id) => (await fetch(`/api/clips/${id}`, { method: 'DELETE' })).status, clipId);
    check('other user delete is 403', st === 403, `status=${st}`);
  }
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (a && clipId) {
    const st = await a.evaluate(async (id) => (await fetch(`/api/clips/${id}`, { method: 'DELETE' })).status, clipId).catch(() => 0);
    if (st) check('maker deletes the clip', st === 204, `status=${st}`);
  }
}
console.log(results.join('\n'));
await browser.close();
