// 방송 예정 시험 (#114): 주인이 시작 예정 시각을 정하면 목록 "곧 시작"과 방송 화면에 보이고, 지우면 사라진다.
// node schedule.mjs [BASE]
import { execFileSync } from 'child_process';
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const LOCAL = /localhost|127.0.0.1/.test(BASE);
// 지난 예정은 API로 만들 수 없어서(앞으로 7일 안만 받음) 로컬에서만 DB로 시각을 바꿔 본다(#136)
const psql = (sql) => execFileSync('docker', ['exec', process.env.E2E_DB_CONTAINER || 'rodlspr-db-1', 'psql', '-U', 'onair', '-d', 'onair', '-tAc', sql], { encoding: 'utf8' }).trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(200); }
  return false;
}
const text = (p, sel) => p.$eval(sel, (e) => (e.hidden ? '' : e.textContent ?? '')).catch(() => '');
async function clickText(p, sel, label) {
  for (const b of await p.$$(sel)) if ((await b.evaluate((x) => x.textContent)) === label) return b.click();
  throw new Error(`no button ${label}`);
}

let owner; let bid;
const title = `예정 시험 ${Date.now().toString(36)}`;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  bid = (await owner.evaluate(async (t) => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: t }) })).json(), title)).id;
  await owner.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.schedule-input');
  check('owner sees schedule tools on ready broadcast', !(await owner.$eval('.schedule-tools', (e) => e.hidden)));
  // 3시간 뒤(이 기기 시각)로 정한다
  await owner.$eval('.schedule-input', (i) => {
    const d = new Date(Date.now() + 3 * 3600 * 1000);
    const p = (n) => String(n).padStart(2, '0');
    i.value = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
  });
  await clickText(owner, '.schedule-tools button', '예정 저장');
  check('owner sees schedule note', await waitFor(async () => (await text(owner, '.schedule-note')).includes('시작 예정'), 5000), await text(owner, '.owner .error'));

  const anon = await (await browser.createBrowserContext()).newPage();
  await anon.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('viewer sees schedule note', await waitFor(async () => (await text(anon, '.schedule-note')).includes('시작 예정'), 5000));
  check('viewer has no schedule tools', !(await anon.$('.schedule-input')));
  await anon.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  check('list shows upcoming', await waitFor(async () => (await text(anon, '.upcoming')).includes(title), 5000), await text(anon, '.upcoming'));
  // 지우면 사라진다
  await clickText(owner, '.schedule-tools button', '예정 지우기');
  check('owner note removed', await waitFor(async () => (await text(owner, '.schedule-note')) === '', 5000));
  await anon.reload({ waitUntil: 'networkidle2' });
  await sleep(800);
  check('list upcoming no longer shows it', !(await text(anon, '.upcoming')).includes(title), await text(anon, '.upcoming'));
  if (LOCAL) {
    // 10분 지난 예정은 '늦어지는 중'으로 남고, 1시간 넘게 지난 예정은 칸에서 빠진다(#136)
    psql(`UPDATE broadcasts SET scheduled_at = now() - interval '10 minutes' WHERE id = ${Number(bid)}`);
    await anon.reload({ waitUntil: 'networkidle2' });
    check('10 min late stays as late', await waitFor(async () => (await text(anon, '.upcoming')).includes('늦어지는 중'), 5000), await text(anon, '.upcoming'));
    psql(`UPDATE broadcasts SET scheduled_at = now() - interval '2 hours' WHERE id = ${Number(bid)}`);
    await anon.reload({ waitUntil: 'networkidle2' });
    await sleep(800);
    check('2 hours late leaves upcoming', !(await text(anon, '.upcoming')).includes(title), await text(anon, '.upcoming'));
  }
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
