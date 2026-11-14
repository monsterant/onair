// 화면 다듬기 시험 (#136): 없는 방송 안내, 보는 사이 끝난 방송의 채팅 입력 칸, 잠깐 멈춘 방송 문구, 입력 칸 이름표.
// node ux3.mjs [BASE]
// 잠깐 멈춘 방송은 실제 송출 없이 만들기 어려워서, 로컬(docker 컨테이너 rodlspr-db-1, 다르면 E2E_DB_CONTAINER)에서만 DB로 상태를 바꿔 본다.
import { execFileSync } from 'child_process';
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const LOCAL = /localhost|127\.0\.0\.1/.test(BASE);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const psql = (sql) => execFileSync('docker', ['exec', process.env.E2E_DB_CONTAINER || 'rodlspr-db-1', 'psql', '-U', 'onair', '-d', 'onair', '-tAc', sql], { encoding: 'utf8' }).trim();
const hasText = (page, t) => page.evaluate((t) => document.body.innerText.includes(t), t);
// 글자가 나타날 때까지 기다린다(고정 시간 기다림은 느린 환경에서 흔들린다)
const waitText = (page, t, ms = 8000) => page.waitForFunction((t) => document.body.innerText.includes(t), { timeout: ms }, t).then(() => true).catch(() => false);

const guest = async () => {
  const p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  return p;
};

let owner; let bid;
try {
  // 1. 입력 칸 이름표(로그인하지 않은 로그인 화면)
  const anon = await (await browser.createBrowserContext()).newPage();
  await anon.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  const labels = await anon.$$eval('form.auth input', (is) => is.map((i) => i.getAttribute('aria-label')));
  check('login inputs have labels', labels.length === 2 && labels.every(Boolean), JSON.stringify(labels));

  // 2. 없는 방송
  await anon.goto(`${BASE}/#/b/999999999`, { waitUntil: 'networkidle2' });
  check('missing broadcast message', await waitText(anon, '없는 방송입니다'));

  // 3. 방송 만들기 화면 이름표
  owner = await guest();
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]', { timeout: 10000 });
  const newLabels = await owner.$$eval('input[name=title], textarea[name=description]', (is) => is.map((i) => i.getAttribute('aria-label')));
  check('new broadcast inputs have labels', newLabels.length === 2 && newLabels.every(Boolean), JSON.stringify(newLabels));
  bid = (await owner.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '화면 다듬기 시험' }) })).json())).id;

  // 4. 잠깐 멈춘 방송 문구(로컬만): 시작한 적이 있는 대기 방송
  const viewer = await guest();
  // 시작한 적 없는 방송은 '아직 방송 전'
  await viewer.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('fresh broadcast says before start', await waitText(viewer, '아직 방송 전입니다'));
  if (LOCAL) {
    psql(`UPDATE broadcasts SET started_at = now() - interval '1 minute' WHERE id = ${Number(bid)}`);
    await viewer.reload({ waitUntil: 'networkidle2' });
    check('paused broadcast message', await waitText(viewer, '잠시 멈췄습니다'));
    check('paused broadcast is not "before start"', !(await hasText(viewer, '아직 방송 전입니다')));
  }

  // 5. 보는 사이에 방송이 끝나면 채팅 입력 칸이 안내로 바뀐다
  await viewer.waitForSelector('form.chat-form');
  const ended = await owner.evaluate(async (id) => (await fetch(`/api/broadcasts/${id}/end`, { method: 'POST' })).status, bid);
  check('owner ended broadcast', ended === 200 || ended === 204, `status=${ended}`);
  // 상세 화면은 5초마다 방송을 다시 읽고, 채팅 칸은 2초마다 본다
  const gone = await viewer.waitForFunction(() => !document.querySelector('form.chat-form'), { timeout: 12000 }).then(() => true).catch(() => false);
  check('chat input replaced after end', gone);
  check('ended chat hint shown', await hasText(viewer, '종료된 방송이라 채팅을 쓸 수 없습니다'));
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
