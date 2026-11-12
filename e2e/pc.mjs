// #1 PC broadcast e2e: owner shares screen (+mic) from the UI, anonymous viewer gets video and audio,
// stop -> ready -> restart -> live, double click guarded, mic-denied cleans up screen tracks,
// navigate away stops publishing, end broadcast hides studio and ends viewer.
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${extra}`);

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: [
    '--use-fake-device-for-media-stream',
    '--use-fake-ui-for-media-stream',
    '--autoplay-policy=no-user-gesture-required',
    '--auto-select-desktop-capture-source=Entire screen',
  ],
});

// 페이지마다 RTCPeerConnection과 화면 공유 트랙을 기록해 둔다(시험에서 통계·정리 여부를 보려고)
const HOOK = () => {
  window.__pcs = [];
  window.__screenTracks = [];
  const Orig = window.RTCPeerConnection;
  window.RTCPeerConnection = function (...a) {
    const pc = new Orig(...a);
    window.__pcs.push(pc);
    return pc;
  };
  window.RTCPeerConnection.prototype = Orig.prototype;
  Object.assign(window.RTCPeerConnection, Orig);
  const gdm = navigator.mediaDevices.getDisplayMedia.bind(navigator.mediaDevices);
  navigator.mediaDevices.getDisplayMedia = async (c) => {
    const s = await gdm(c);
    window.__screenTracks.push(...s.getTracks());
    return s;
  };
};

async function newPage(ctx) {
  const p = await ctx.newPage();
  await p.evaluateOnNewDocument(HOOK);
  p.on('dialog', (d) => d.accept());
  return p;
}

const pubState = (p) => p.$eval('.pub-state', (e) => e.textContent).catch(() => null);
const apiStatus = (p, id) => p.evaluate(async (id) => (await (await fetch(`/api/broadcasts/${id}`)).json()).status, id);
async function waitFor(fn, want, ms = 15000) {
  const t0 = Date.now();
  let v;
  while (Date.now() - t0 < ms) {
    v = await fn();
    if (v === want) return v;
    await sleep(500);
  }
  return v;
}
// 시청 쪽: 받은 소리·영상 바이트
const inbound = (p) =>
  p.evaluate(async () => {
    const out = { audio: 0, video: 0 };
    for (const pc of window.__pcs) {
      if (pc.connectionState === 'closed') continue;
      (await pc.getStats()).forEach((r) => {
        if (r.type === 'inbound-rtp') out[r.kind] += r.bytesReceived ?? 0;
      });
    }
    return out;
  });
// 송출 쪽: 보낸 트랙 종류
const outboundKinds = (p) =>
  p.evaluate(async () => {
    const kinds = [];
    for (const pc of window.__pcs) {
      if (pc.connectionState === 'closed') continue;
      (await pc.getStats()).forEach((r) => {
        if (r.type === 'outbound-rtp' && (r.bytesSent ?? 0) > 0) kinds.push(r.kind);
      });
    }
    return kinds.sort().join(',');
  });

try {
  const ownerCtx = await browser.createBrowserContext();
  const owner = await newPage(ownerCtx);
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button:nth-child(1)');
  await sleep(1000);
  // 이전 시험이 남긴 방송이 있으면 5개 상한에 걸리지 않게 정리
  await owner.evaluate(async () => {
    const me = await (await fetch('/api/auth/me')).json();
    for (const b of await (await fetch('/api/broadcasts')).json())
      if (b.owner_id === me.id) await fetch(`/api/broadcasts/${b.id}`, { method: 'DELETE' });
  });
  const id = await owner.evaluate(async () => {
    const r = await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: 'PC 송출 시험' }) });
    return (await r.json()).id;
  });
  await owner.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  check('studio panel shown to owner', true);

  // 다른 사용자·비로그인에게는 방송하기 칸이 없다
  const viewerCtx = await browser.createBrowserContext();
  const viewer = await newPage(viewerCtx);
  await viewer.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await sleep(500);
  check('viewer has no studio', !(await viewer.$('.studio')));

  // 1) 화면 공유로 시작. 두 번 눌러도 하나만 시작된다.
  await owner.click('.start-screen');
  await owner.click('.start-screen').catch(() => {});
  const s1 = await waitFor(() => pubState(owner), '송출 중');
  check('publishing live', s1 === '송출 중', s1);
  // 보내는 소리 크기 막대가 보이고 움직인다(#80, 가짜 마이크는 주기적으로 삐 소리를 낸다)
  check('sound level meter moves', await (async () => { const end = Date.now() + 8000; while (Date.now() < end) { const v = await owner.$eval('.level', (e) => (e.hidden ? -1 : Number(e.dataset.level ?? 0))); if (v > 0) return true; await sleep(200); } return false; })(), await owner.$eval('.level', (e) => e.outerHTML).catch(() => 'none'));
  check('preview visible', await owner.$eval('.preview', (v) => !v.hidden && !!v.srcObject));
  check('only one peer connection', (await owner.evaluate(() => window.__pcs.filter((p) => p.connectionState !== 'closed').length)) === 1);
  const kinds = await waitFor(() => outboundKinds(owner), 'audio,video', 5000);
  check('sends one video + one (mixed) audio', kinds === 'audio,video', kinds);
  const noteText = await owner.$eval('.studio', (e) => e.innerText);
  check('no 409 error', !noteText.includes('이미 이 방송'), '');
  check('status live', (await waitFor(() => apiStatus(owner, id), 'live')) === 'live');

  // 2) 비로그인 시청자: 영상 재생 + 소리 바이트
  await sleep(8000);
  const t1 = await viewer.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
  const in1 = await inbound(viewer);
  await sleep(3000);
  const t2 = await viewer.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
  const in2 = await inbound(viewer);
  check('viewer video plays', t2 > t1 && t1 >= 0, `${t1} -> ${t2}`);
  check('viewer receives audio', in2.audio > in1.audio && in2.audio > 0, JSON.stringify(in2));

  // 3) 송출 멈춤 → 장치 꺼짐, ready → 다시 시작 → live
  await owner.click('.stop-publish');
  const s2 = await waitFor(() => pubState(owner), '송출하지 않음');
  check('stop -> idle', s2 === '송출하지 않음', s2);
  check('screen tracks ended after stop', await owner.evaluate(() => window.__screenTracks.every((t) => t.readyState === 'ended')));
  check('status ready after stop', (await waitFor(() => apiStatus(owner, id), 'ready')) === 'ready');
  await owner.click('.start-screen');
  check('restart live', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  check('status live again', (await waitFor(() => apiStatus(owner, id), 'live')) === 'live');

  // 4) 다른 화면으로 가면 송출을 멈춘다
  await owner.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' });
  check('navigate away -> ready', (await waitFor(() => apiStatus(owner, id), 'ready')) === 'ready');
  check('tracks ended after navigate', await owner.evaluate(() => window.__screenTracks.every((t) => t.readyState === 'ended')));

  // 5) 마이크 거부 → 화면만으로 계속 송출하고 이유를 경고로 보여 준다(#67에서 바뀜: 전에는 시작 전체가 실패)
  await owner.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  await owner.evaluate(() => {
    navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('denied', 'NotAllowedError'));
    window.__screenTracks = [];
  });
  await owner.click('.start-screen');
  check('mic denied: screen still goes live', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  const denied = await owner.$eval('.studio .warn', (e) => e.textContent);
  check('mic denied warning', denied.includes('마이크를 쓸 수 없어') && denied.includes('권한이 거부'), denied);
  await owner.click('.stop-publish');
  check('screen tracks cleaned after stop', (await waitFor(() => pubState(owner), '송출하지 않음')) === '송출하지 않음' && await owner.evaluate(() => window.__screenTracks.length > 0 && window.__screenTracks.every((t) => t.readyState === 'ended')));

  // 6) 마이크 끄고(체크 해제) 화면만으로 송출 → 방송 종료 → 방송하기 칸 숨김, 시청자 "종료"
  await owner.reload({ waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-screen');
  await owner.click('#with-mic');
  await owner.click('.start-screen');
  check('screen-only publish live', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  for (const b of await owner.$$('.owner button')) if ((await b.evaluate((x) => x.textContent)) === '방송 종료') await b.click();
  check('status ended', (await waitFor(() => apiStatus(owner, id), 'ended')) === 'ended');
  await sleep(1000);
  await sleep(3000);
  check('studio hidden after end', await owner.$eval('.studio', (e) => e.hidden));
  check('tracks ended after end', await owner.evaluate(() => window.__screenTracks.every((t) => t.readyState === 'ended')));
  await sleep(6000);
  check('viewer sees ended', (await viewer.$eval('main', (m) => m.innerText)).includes('종료된 방송'));

  await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), id);
} catch (e) {
  check('no exception', false, e.stack);
}
console.log(results.join('\n'));
await browser.close();
