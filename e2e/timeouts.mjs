// 송출 시간 제한 시험 (#31 P11): 1) 송출 요청에 답이 없음 2) 영상 연결이 안 됨 3) 화면 공유 중 마이크 꺼짐 4) 송출 중 연결이 오래 끊김
// node timeouts.mjs [BASE] [--pause-ome 컨테이너이름]   (4번은 로컬에서 OME 컨테이너를 잠시 멈춰 끊김을 만든다)
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';
import { execSync } from 'node:child_process';

const args = process.argv.slice(2);
const pi = args.indexOf('--pause-ome');
const ome = pi >= 0 ? args.splice(pi, 2)[1] : null;
const BASE = (args[0] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${ok ? '' : extra}`);
const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--auto-select-desktop-capture-source=Entire screen', '--autoplay-policy=no-user-gesture-required'],
});
async function waitFor(fn, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return true; await sleep(300); }
  return false;
}
const text = (p, sel) => p.$eval(sel, (e) => e.textContent ?? '').catch(() => '');
const pubState = (p) => p.$eval('.pub-state', (e) => e.className);
const isLive = async (p) => (await pubState(p)).includes('live');
const isIdle = async (p) => (await pubState(p)).includes('idle');
const note = (p) => text(p, '.studio .hint[role=status]');

let owner; let bid;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  // __noConnect면 영상 통로 후보를 모으지 못하게 해서(TURN만 쓰라 하고 TURN 서버는 안 줌) 연결이 되지 않게 한다
  await owner.evaluateOnNewDocument(() => {
    window.__noConnect = false;
    const Orig = window.RTCPeerConnection;
    window.RTCPeerConnection = function (cfg) {
      return new Orig(window.__noConnect ? { ...(cfg ?? {}), iceTransportPolicy: 'relay', iceServers: [] } : cfg);
    };
    window.RTCPeerConnection.prototype = Orig.prototype;
    Object.assign(window.RTCPeerConnection, Orig);
    window.RTCPeerConnection.generateCertificate = Orig.generateCertificate;
    // 마이크 트랙을 모아 둔다(시험에서 ended를 보내려고)
    window.__mics = [];
    const gum = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (c) => {
      const s = await gum(c);
      if (c && c.audio && !c.video) window.__mics.push(...s.getAudioTracks());
      return s;
    };
  });
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '시간 제한 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  await owner.waitForSelector('.start-screen');

  // 1) 송출 요청(WHIP)에 서버가 답하지 않음 → 15초 뒤 멈추고 이유
  await owner.setRequestInterception(true);
  let hang = true;
  owner.on('request', (r) => {
    if (r.isInterceptResolutionHandled()) return;
    if (hang && r.method() === 'POST' && r.url().includes('direction=whip')) return void setTimeout(() => r.abort().catch(() => {}), 30_000);
    r.continue().catch(() => {});
  });
  const t0 = Date.now();
  await owner.click('.start-screen');
  const gaveUp = await waitFor(() => isIdle(owner).then(async (i) => i && (await note(owner)).includes('15초 동안 답하지')), 25_000);
  const took = Math.round((Date.now() - t0) / 1000);
  check('whip no answer -> stops with reason after ~15s', gaveUp && took >= 14 && took <= 22, `${took}s ${await note(owner)}`);
  check('buttons usable after whip timeout', await owner.$eval('.start-screen', (b) => !b.disabled));
  hang = false;

  // 2) 송출은 받아들여졌는데 영상 연결이 안 됨 → 20초 뒤 멈추고 이유
  await owner.evaluate(() => { window.__noConnect = true; });
  await sleep(1000);
  const t1 = Date.now();
  await owner.click('.start-screen');
  const gaveUp2 = await waitFor(() => isIdle(owner).then(async (i) => i && (await note(owner)).includes('20초 안에')), 30_000);
  const took2 = Math.round((Date.now() - t1) / 1000);
  check('no media connection -> stops with reason after ~20s', gaveUp2 && took2 >= 18 && took2 <= 28, `${took2}s ${await note(owner)}`);
  await owner.evaluate(() => { window.__noConnect = false; });
  await sleep(2000);

  // 3) 정상 송출, 화면 공유 중 마이크가 꺼짐 → 경고, 송출은 계속
  await owner.click('.start-screen');
  check('normal publish goes live', await waitFor(() => isLive(owner), 15_000), await note(owner));
  check('no mic warning while mic works', !(await text(owner, '.studio .warn')).includes('마이크가 꺼졌습니다'));
  // 마이크를 뺀 것처럼 마이크 트랙에 ended를 보낸다(장치가 사라지면 브라우저가 보내는 이벤트)
  check('mic track captured', await owner.evaluate(() => (window.__mics ?? []).length > 0));
  await owner.evaluate(() => window.__mics.forEach((t) => t.dispatchEvent(new Event('ended'))));
  check('mic lost warning shown', await waitFor(async () => (await text(owner, '.studio .warn')).includes('마이크가 꺼졌습니다'), 3000), await text(owner, '.studio .warn'));
  check('still live after mic lost', await isLive(owner));

  // 4) 송출 중 연결이 오래 끊김(OME 컨테이너를 잠시 멈춤) → 15초쯤 뒤 멈추고 이유
  if (ome) {
    execSync(`docker pause ${ome}`);
    const t2 = Date.now();
    // 서버는 아직 이 방송을 "방송 중"으로 본다(OME가 멈춰 닫힘 알림이 없음). 그래서 바로 멈추지 않고 15초를 기다리거나
    // 브라우저가 실패로 판단한 뒤 멈춰야 한다. "서버에서 끊겼다" 문구가 나오면 잘못 멈춘 것이다(#136 리뷰).
    const gaveUp3 = await waitFor(() => isIdle(owner).then(async (i) => i && /15초 넘게|연결이 끊겨/.test(await note(owner))), 60_000);
    const took3 = Math.round((Date.now() - t2) / 1000);
    execSync(`docker unpause ${ome}`);
    check('lost connection -> stops with reason within 45s', gaveUp3 && took3 <= 45, `${took3}s ${await note(owner)}`);
    check('lost connection is not reported as a server stop', !(await note(owner)).includes('서버에서'), await note(owner));
    results.push(`INFO lost connection stop took ${took3}s (disconnected 감지 시간 + 15초)`);
  }
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (ome) { try { execSync(`docker unpause ${ome}`, { stdio: 'ignore' }); } catch {} }
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
