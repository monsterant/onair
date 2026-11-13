// 후원 순위 시험 (#99): 후원이 없으면 순위 칸이 숨고, 적립된 후원이 있으면 합계가 큰 사람부터 보인다.
// node ranking.mjs [BASE]
// 결제를 끝까지 할 수 없어서, 로컬(docker 컨테이너 rodlspr-db-1)에서만 DB에 적립된 후원을 직접 넣어 본다.
// 운영 주소에서는 DB를 건드리지 않고 "후원이 없으면 숨김"만 본다.
import { execFileSync } from 'child_process';
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const LOCAL = /localhost|127\.0\.0\.1/.test(BASE);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const psql = (sql) => execFileSync('docker', ['exec', 'rodlspr-db-1', 'psql', '-U', 'onair', '-d', 'onair', '-tAc', sql], { encoding: 'utf8' }).trim();

let p; let bid; let tag;
try {
  p = await (await browser.createBrowserContext()).newPage();
  p.on('dialog', (d) => d.accept());
  await p.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await p.waitForSelector('.demo .row button');
  await p.click('.demo .row button');
  await sleep(1500);
  bid = (await p.evaluate(async () => (await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '순위 시험' }) })).json())).id;
  const anon = await (await browser.createBrowserContext()).newPage();
  await anon.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  await anon.waitForFunction(() => document.querySelector('.donation-total')?.textContent?.includes('건'), { timeout: 10000 });
  check('no donations: ranking hidden', await anon.$eval('.donor-top', (e) => e.hidden));
  if (LOCAL) {
    // 후원자 2명을 만들고 적립된 후원을 넣는다: A 1000+5000, B 3000
    tag = Date.now().toString(36);
    const a = psql(`INSERT INTO users (username, password_hash) VALUES ('rk_a_${tag}', 'x') RETURNING id`).split('\n')[0];
    const b = psql(`INSERT INTO users (username, password_hash) VALUES ('rk_b_${tag}', 'x') RETURNING id`).split('\n')[0];
    for (const [i, u, amt] of [[1, a, 1000], [2, b, 3000], [3, a, 5000]]) {
      psql(`INSERT INTO donations (order_id, broadcast_id, donor_id, amount, status, payment_key, paid_at) VALUES ('rk-${tag}-${i}', ${bid}, ${u}, ${amt}, 'paid', 'rk-${tag}-${i}', now())`);
    }
    await anon.reload({ waitUntil: 'networkidle2' });
    await anon.waitForFunction(() => document.querySelector('.donation-total')?.textContent?.includes('3건'), { timeout: 10000 });
    const rows = await anon.$$eval('.donor-top li', (ls) => ls.map((l) => l.textContent));
    check('ranking shown, biggest first', rows.length === 2 && rows[0].startsWith(`rk_a_${tag}`) && rows[0].includes('6,000원') && rows[0].includes('2번') && rows[1].startsWith(`rk_b_${tag}`), JSON.stringify(rows));
    check('ranking visible', !(await anon.$eval('.donor-top', (e) => e.hidden)));
  } else {
    results.push('SKIP ranking with donations (운영에서는 DB에 후원을 넣지 않음)');
  }
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (p && bid) await p.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
  // 중간에 실패해도 시험 사용자를 지운다(#100)
  if (tag) try { psql(`DELETE FROM users WHERE username IN ('rk_a_${tag}', 'rk_b_${tag}')`); } catch {}
}
console.log(results.join('\n'));
await browser.close();
