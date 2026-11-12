// #2 mobile camera broadcast e2e (phone emulation: 375px, touch, mobile UA, no getDisplayMedia):
// camera publish, anonymous viewer video+audio, mirror preview, stop/restart, camera track ended -> stop with reason,
// camera denied -> message + cleanup, back camera, end, no horizontal scroll.
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, extra = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name} ${extra}`);

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required'],
});

const PHONE_UA = 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36';
const HOOK = (noScreen) => {
  window.__pcs = [];
  window.__camTracks = [];
  const O = window.RTCPeerConnection;
  window.RTCPeerConnection = function (...a) { const pc = new O(...a); window.__pcs.push(pc); return pc; };
  window.RTCPeerConnection.prototype = O.prototype;
  Object.assign(window.RTCPeerConnection, O);
  const gum = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
  navigator.mediaDevices.getUserMedia = async (c) => { const s = await gum(c); window.__camTracks.push(...s.getTracks()); window.__lastConstraints = c; return s; };
  // 휴대폰 브라우저처럼 화면 공유가 없다
  if (noScreen) navigator.mediaDevices.getDisplayMedia = undefined;
};

async function phonePage(ctx, noScreen = true) {
  const p = await ctx.newPage();
  await p.setUserAgent(PHONE_UA);
  await p.setViewport({ width: 375, height: 740, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
  await p.evaluateOnNewDocument(HOOK, noScreen);
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
const inbound = (p) =>
  p.evaluate(async () => {
    const out = { audio: 0, video: 0 };
    for (const pc of window.__pcs) {
      if (pc.connectionState === 'closed') continue;
      (await pc.getStats()).forEach((r) => { if (r.type === 'inbound-rtp') out[r.kind] += r.bytesReceived ?? 0; });
    }
    return out;
  });
const outboundKinds = (p) =>
  p.evaluate(async () => {
    const kinds = [];
    for (const pc of window.__pcs) {
      if (pc.connectionState === 'closed') continue;
      (await pc.getStats()).forEach((r) => { if (r.type === 'outbound-rtp' && (r.bytesSent ?? 0) > 0) kinds.push(r.kind); });
    }
    return kinds.sort().join(',');
  });
const noHScroll = (p) => p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1);
const allEnded = (p) => p.evaluate(() => window.__camTracks.every((t) => t.readyState === 'ended'));

try {
  const ownerCtx = await browser.createBrowserContext();
  const owner = await phonePage(ownerCtx);
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  check('login page fits phone width', await noHScroll(owner));
  await owner.click('.demo .row button:nth-child(1)');
  await sleep(1000);
  await owner.evaluate(async () => {
    const me = await (await fetch('/api/auth/me')).json();
    for (const b of await (await fetch('/api/broadcasts')).json())
      if (b.owner_id === me.id) await fetch(`/api/broadcasts/${b.id}`, { method: 'DELETE' });
  });
  const id = await owner.evaluate(async () => {
    const r = await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '휴대폰 송출 시험' }) });
    return (await r.json()).id;
  });
  await owner.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-camera');
  check('screen-share row hidden on phone', await owner.$eval('.screen-mode', (e) => e.hidden));
  check('camera button enabled', await owner.$eval('.start-camera', (b) => !b.disabled));
  check('detail page fits phone width', await noHScroll(owner));
  check('list page fits phone width', await (async () => { const p = await phonePage(ownerCtx); await p.goto(`${BASE}/#/`, { waitUntil: 'networkidle2' }); await sleep(500); const ok = await noHScroll(p); await p.close(); return ok; })());
  // 위에서 연 탭 때문에 방송자 탭이 숨겨진 상태로 남을 수 있다. 숨겨진 채로 카메라를 켜면 바로 멈춘다(#33 2바퀴, 의도한 동작)
  await owner.bringToFront();
  await sleep(300);

  // 시청자는 다른 브라우저(다른 기기)에서 연다. 같은 브라우저에 탭을 열면 방송자 탭이 숨겨져 카메라 송출이 멈춘다(#33 M1, 의도한 동작).
  const viewerBrowser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--autoplay-policy=no-user-gesture-required'] });
  const viewerCtx = await viewerBrowser.createBrowserContext();
  globalThis.__vb = viewerBrowser;
  const viewer = await phonePage(viewerCtx);
  await viewer.goto(`${BASE}/#/b/${id}`, { waitUntil: 'networkidle2' });

  // 1) 앞 카메라로 시작
  await owner.tap('.start-camera');
  {
    const st = await waitFor(() => pubState(owner), '송출 중');
    const studio = await owner.$eval('.studio', (e) => e.innerText.split('\n').join(' | '));
    const vis = await owner.evaluate(() => document.visibilityState);
    check('camera publishing live', st === '송출 중', JSON.stringify({ st, studio, vis }));
  }
  check('front camera requested', await owner.evaluate(() => window.__lastConstraints?.video?.facingMode === 'user'));
  check('preview mirrored for front camera', await owner.$eval('.preview', (v) => !v.hidden && v.classList.contains('mirror')));
  const kinds = await waitFor(() => outboundKinds(owner), 'audio,video', 5000);
  check('sends video + audio', kinds === 'audio,video', kinds);
  check('status live', (await waitFor(() => apiStatus(owner, id), 'live')) === 'live');
  check('detail fits phone width while live', await noHScroll(owner));

  // 2) 휴대폰 시청자(비로그인): 영상·소리
  await sleep(8000);
  const t1 = await viewer.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
  const i1 = await inbound(viewer);
  await sleep(3000);
  const t2 = await viewer.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
  const i2 = await inbound(viewer);
  check('phone viewer video plays', t2 > t1 && t1 >= 0, `${t1} -> ${t2}`);
  check('phone viewer receives audio', i2.audio > i1.audio && i2.audio > 0, JSON.stringify(i2));
  check('viewer fits phone width', await noHScroll(viewer));

  // 3) 멈춤 → 장치 꺼짐, ready → 뒤 카메라로 다시 시작
  await owner.tap('.stop-publish');
  check('stop -> idle', (await waitFor(() => pubState(owner), '송출하지 않음')) === '송출하지 않음');
  check('camera tracks ended after stop', await allEnded(owner));
  check('status ready', (await waitFor(() => apiStatus(owner, id), 'ready')) === 'ready');
  await owner.select('.facing', 'environment');
  await owner.tap('.start-camera');
  check('back camera live', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  check('back camera requested', await owner.evaluate(() => window.__lastConstraints?.video?.facingMode === 'environment'));
  check('back camera preview not mirrored', await owner.$eval('.preview', (v) => !v.classList.contains('mirror')));

  // 4) 카메라가 꺼지면(다른 앱으로 감 등) 송출이 멈추고 이유가 보인다
  await owner.evaluate(() => {
    const v = window.__camTracks.find((t) => t.kind === 'video' && t.readyState === 'live');
    v.stop();
    v.dispatchEvent(new Event('ended'));
  });
  check('camera ended -> idle', (await waitFor(() => pubState(owner), '송출하지 않음')) === '송출하지 않음');
  check('camera ended reason shown', (await owner.$eval('.studio', (e) => e.innerText)).includes('카메라가 꺼져'));
  check('all tracks ended', await allEnded(owner));
  check('status ready after camera ended', (await waitFor(() => apiStatus(owner, id), 'ready')) === 'ready');

  // 5) 카메라 권한 거부
  await owner.evaluate(() => { navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('denied', 'NotAllowedError')); });
  await owner.tap('.start-camera');
  await sleep(1500);
  check('camera denied message', (await owner.$eval('.studio', (e) => e.innerText)).includes('권한이 거부'));
  check('idle after denied', (await pubState(owner)) === '송출하지 않음');
  check('buttons usable again', await owner.$eval('.start-camera', (b) => !b.disabled));

  // 6) 다시 송출 → 방송 종료
  await owner.reload({ waitUntil: 'networkidle2' });
  await owner.waitForSelector('.start-camera');
  await owner.tap('.start-camera');
  check('live again', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  for (const b of await owner.$$('.owner button')) if ((await b.evaluate((x) => x.textContent)) === '방송 종료') await b.tap();
  check('status ended', (await waitFor(() => apiStatus(owner, id), 'ended')) === 'ended');
  await sleep(3500);
  check('studio hidden after end', await owner.$eval('.studio', (e) => e.hidden));
  check('tracks ended after end', await allEnded(owner));
  await sleep(5000);
  check('viewer sees ended', (await viewer.$eval('main', (m) => m.innerText)).includes('종료된 방송'));

  await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), id);

  // 7) 송출 중 카메라 mute → 경고, 마이크 끊김 → 경고, 화면 숨김 → 송출 멈춤(#33)
  const id2 = await owner.evaluate(async () => {
    const r = await fetch('/api/broadcasts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ title: '숨김 시험' }) });
    return (await r.json()).id;
  });
  await owner.goto(`${BASE}/#/b/${id2}`, { waitUntil: 'networkidle2' });
  await owner.waitForFunction(() => document.querySelector('.detail-head h2')?.textContent === '숨김 시험' && document.querySelector('.start-camera'));
  await owner.tap('.start-camera');
  check('live for hidden test', (await waitFor(() => pubState(owner), '송출 중')) === '송출 중');
  await owner.evaluate(() => { const v = window.__camTracks.filter((t) => t.kind === 'video' && t.readyState === 'live').pop(); v.dispatchEvent(new Event('mute')); });
  check('camera mute warns', (await owner.$eval('.warn', (e) => e.textContent)).includes('잠시 멈췄습니다'));
  await owner.evaluate(() => { const a = window.__camTracks.filter((t) => t.kind === 'audio' && t.readyState === 'live').pop(); a.dispatchEvent(new Event('ended')); });
  check('mic ended warns', (await owner.$eval('.warn', (e) => e.textContent)).includes('마이크가 꺼졌습니다'));
  check('still live after mic ended', (await pubState(owner)) === '송출 중');
  await owner.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => true });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  check('hidden -> idle', (await waitFor(() => pubState(owner), '송출하지 않음')) === '송출하지 않음');
  check('hidden reason shown', (await owner.$eval('.studio', (e) => e.innerText)).includes('화면을 떠나'));
  check('tracks ended after hidden', await allEnded(owner));
  check('status ready after hidden', (await waitFor(() => apiStatus(owner, id2), 'ready')) === 'ready');
  await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), id2);
} catch (e) {
  check('no exception', false, e.stack);
}
console.log(results.join('\n'));
await browser.close();
await (globalThis.__vb?.close?.() ?? Promise.resolve());
