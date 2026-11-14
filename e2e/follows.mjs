// 방송자 즐겨찾기 시험 (#102): 방송 화면에서 즐겨찾기·해제, 목록 위 "즐겨찾는 방송자" 칸, 주인·비로그인에게는 버튼 없음.
// node follows.mjs [BASE]
// 방송 중 표시는 실제 송출 없이 보기 어려워서 로컬(docker DB 컨테이너, e2e/local-db.mjs)에서만 방송 상태를 직접 바꿔 본다.
import { execFileSync } from 'child_process';
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';
import { DB_CONTAINER } from './local-db.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const LOCAL = /localhost|127\.0\.0\.1/.test(BASE);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const psql = (sql) => execFileSync('docker', ['exec', DB_CONTAINER, 'psql', '-U', 'onair', '-d', 'onair', '-tAc', sql], { encoding: 'utf8' }).trim();
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(200); }
  return false;
}
async function guest() {
  const p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
}
const text = (p, sel) => p.$eval(sel, (e) => e.textContent ?? '').catch(() => '');

let owner; let bid;
try {
  owner = await guest();
  const ownerName = (await owner.evaluate(async () => (await fetch('/api/auth/me')).json())).username;
  bid = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '즐겨찾기 시험' }) })).json())).id;
  await owner.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.owner-line');
  check('owner has no follow button', !(await owner.$('.owner-line .follow')));
  const anon = await (await browser.createBrowserContext()).newPage();
  await anon.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await anon.waitForSelector('.owner-line');
  check('anonymous has no follow button', !(await anon.$('.owner-line .follow')));

  const fan = await guest();
  await fan.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  await sleep(500);
  check('no follows: section hidden', await fan.$eval('.follows', (e) => e.hidden));
  await fan.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await fan.waitForSelector('.owner-line .follow:not([disabled])');
  check('follow button starts off', (await text(fan, '.owner-line .follow')) === '즐겨찾기');
  await fan.click('.owner-line .follow');
  check('click follows', await waitFor(async () => (await text(fan, '.owner-line .follow')) === '즐겨찾기 해제', 5000), await text(fan, '.owner-line .follow'));
  // 다시 들어와도 상태가 남는다(서버에 저장)
  await fan.reload({ waitUntil: 'networkidle2' });
  check('follow kept after reload', await waitFor(async () => (await text(fan, '.owner-line .follow')) === '즐겨찾기 해제', 5000));
  await fan.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  check('list shows followed streamer resting', await waitFor(async () => (await text(fan, '.follows')).includes(ownerName) && (await text(fan, '.follows')).includes('쉬는 중'), 5000), await text(fan, '.follows'));
  if (LOCAL) {
    psql(`UPDATE broadcasts SET status = 'live', started_at = now() WHERE id = ${Number(bid)}`);
    await fan.reload({ waitUntil: 'networkidle2' });
    check('live streamer links to broadcast', await waitFor(async () => (await fan.$eval('.follows a', (a) => a.getAttribute('href')).catch(() => '')) === `#/b/${bid}`, 5000), await text(fan, '.follows'));
    psql(`UPDATE broadcasts SET status = 'ended', ended_at = now() WHERE id = ${Number(bid)}`);
  } else {
    results.push('SKIP live streamer link (운영에서는 방송 상태를 바꾸지 않음)');
  }
  // 해제하면 칸이 사라진다
  await fan.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await fan.waitForSelector('.owner-line .follow:not([disabled])');
  await fan.click('.owner-line .follow');
  await waitFor(async () => (await text(fan, '.owner-line .follow')) === '즐겨찾기', 5000);
  await fan.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  await sleep(800);
  check('unfollow hides section', await fan.$eval('.follows', (e) => e.hidden));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
