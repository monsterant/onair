// auth smoke test (after #3 UI): signup, reload keeps login, cookie flags, logout, wrong password, login, demo1/demo2.
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${extra}`);

const browser = await puppeteer.launch({ executablePath: CHROME, headless: true });
const page = await browser.newPage();
const who = () => page.$eval('.who', (e) => e.innerText);
const user = `e2e_${Date.now() % 1e8}`;
const toLogin = async () => {
  await page.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await page.waitForSelector('input[name=username]');
};
const logoutBtn = async () => {
  for (const b of await page.$$('.who button')) if ((await b.evaluate((x) => x.textContent)) === '로그아웃') await b.click();
  await sleep(1000);
};

try {
  await toLogin();
  check('logged out at start', (await who()).includes('로그인'), await who());
  // #55: 정상이면 하단 상태 문구를 보이지 않는다(문제가 있을 때만)
  check('health ok shows no footer text', (await page.$eval('.status', (e) => e.innerText)) === '');
  await page.type('input[name=username]', user);
  await page.type('input[name=password]', 'password123');
  await page.click('.auth .row button[type=button]');
  await sleep(1500);
  check('signup logs in', (await who()).includes(`${user}님`), await who());
  await page.reload({ waitUntil: 'networkidle2' });
  await sleep(500);
  check('reload keeps login', (await who()).includes(`${user}님`));
  const c = (await page.cookies()).find((x) => x.name === 'onair_session');
  check('cookie httpOnly+lax+secure', !!c && c.httpOnly && c.sameSite === 'Lax' && c.secure);
  check('script cannot read cookie', !(await page.evaluate(() => document.cookie.includes('onair_session'))));
  await logoutBtn();
  check('logout', !(await who()).includes('님'));
  await toLogin();
  await page.type('input[name=username]', user);
  await page.type('input[name=password]', 'wrongpass99');
  await page.click('.auth button[type=submit]');
  await sleep(1500);
  check('wrong password shows error', (await page.$eval('.auth .error', (e) => e.innerText)).includes('틀렸습니다'));
  await page.$eval('input[name=password]', (e) => { e.value = ''; });
  await page.type('input[name=password]', 'password123');
  await page.click('.auth button[type=submit]');
  await sleep(1500);
  check('login', (await who()).includes(`${user}님`));
  await logoutBtn();
  // #35: 테스트 계정 버튼은 하나이고, 누를 때마다 새 계정(guest_ + 16진수 12자)
  const seen = [];
  for (let i = 0; i < 2; i++) {
    await toLogin();
    check('one guest button', (await page.$$('.demo .row button')).length === 1);
    await page.click('.demo .row button');
    await sleep(1500);
    const w = await who();
    const m = w.match(/guest_[0-9a-f]{12}/);
    check(`guest login ${i}`, !!m, w);
    if (m) seen.push(m[0]);
    await logoutBtn();
  }
  check('guest accounts differ', seen.length === 2 && seen[0] !== seen[1], seen.join(','));
} catch (e) {
  check('no exception', false, e.message);
}
console.log(results.join('\n'));
await browser.close();
