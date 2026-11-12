// 송출 장치 다듬기 시험 (#67, 재수정 #68): 마이크 없이 송출, 영상 비율에 맞춘 시청 칸, 송출 중 카메라 전환
// node devices.mjs [BASE]
import puppeteer from 'puppeteer-core';
import { CHROME } from './chrome.mjs';

const BASE = (process.argv[2] ?? 'http://localhost:5173/').replace(/\/$/, '');
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
const videoTime = (p) => p.evaluate(() => document.querySelector('#player video')?.currentTime ?? -1);
async function playing(p, ms) {
  return waitFor(async () => { const a = await videoTime(p); await sleep(700); const b = await videoTime(p); return a >= 0 && b > a; }, ms);
}
const text = (p, sel) => p.$eval(sel, (e) => e.textContent ?? '').catch(() => '');
const pubState = (p) => p.$eval('.pub-state', (e) => e.className);
const isLive = async (p) => (await pubState(p)).includes('live');
const isIdle = async (p) => (await pubState(p)).includes('idle');

// 기기 흉내. window.__m으로 바꾼다.
//  audio: 'notfound' | 'notallowed' | 'ok' | 'readable-once'(영상+소리 함께 요청한 첫 번째만 NotReadableError)
//  video: 'ok' | 'fail' / cams: 카메라 수 / perm: permissions.query 결과(없으면 TypeError) / delay: 영상 켜기 지연(ms)
function mockDevices() {
  window.__m = { audio: 'notfound', video: 'ok', cams: 2, perm: null, delay: 0 };
  window.__calls = [];
  window.__live = [];
  let n = 0;
  const canvasTrack = (want) => {
    const cv = document.createElement('canvas');
    cv.width = 360; cv.height = 640;
    const ctx = cv.getContext('2d');
    let k = 0;
    setInterval(() => { ctx.fillStyle = `hsl(${k++ % 360},80%,50%)`; ctx.fillRect(0, 0, 360, 640); }, 33);
    const t = cv.captureStream(30).getVideoTracks()[0];
    // PC 웹캠 흉내: 방향 정보 없음. 장치 번호(exact)를 주면 그 카메라, 아니면 기본 카메라(cam0)
    const id = want ?? 'cam0';
    n++;
    t.getSettings = () => ({ deviceId: id, width: 360, height: 640 });
    return t;
  };
  const audioTrack = () => {
    const ac = new AudioContext();
    const o = ac.createOscillator();
    const d = ac.createMediaStreamDestination();
    o.connect(d); o.start();
    return d.stream.getAudioTracks()[0];
  };
  navigator.mediaDevices.enumerateDevices = async () =>
    Array.from({ length: window.__m.cams }, (_, i) => ({ kind: 'videoinput', deviceId: `cam${i}`, label: `cam${i}` }));
  navigator.permissions.query = async ({ name }) => {
    if (!window.__m.perm) throw new TypeError('not supported');
    return { state: window.__m.perm[name] ?? 'prompt' };
  };
  navigator.mediaDevices.getUserMedia = async (c) => {
    const kind = c.video && c.audio ? 'video+audio' : c.video ? 'video' : 'audio';
    window.__calls.push(kind);
    if (c.audio) {
      const a = window.__m.audio;
      if (a === 'readable-once' && kind === 'video+audio' && !window.__readableDone) { window.__readableDone = true; throw new DOMException('busy', 'NotReadableError'); }
      if (a === 'notfound') throw new DOMException('no mic', 'NotFoundError');
      if (a === 'notallowed') throw new DOMException('denied', 'NotAllowedError');
    }
    const tracks = [];
    if (c.video) {
      if (window.__m.delay) await new Promise((r) => setTimeout(r, window.__m.delay));
      if (window.__m.video === 'fail') throw new DOMException('busy', 'NotReadableError');
      tracks.push(canvasTrack(c.video.deviceId?.exact));
    }
    if (c.audio) tracks.push(audioTrack());
    window.__live.push(...tracks);
    return new MediaStream(tracks);
  };
}
const liveTracks = (p) => p.evaluate(() => window.__live.filter((t) => t.readyState === 'live').length);

let owner; let bid;
try {
  owner = await (await browser.createBrowserContext()).newPage();
  owner.on('dialog', (d) => d.accept());
  await owner.evaluateOnNewDocument(mockDevices);
  await owner.goto(`${BASE}/#/login`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('.demo .row button');
  await owner.click('.demo .row button');
  await sleep(1500);
  await owner.goto(`${BASE}/#/new`, { waitUntil: 'networkidle2' });
  await owner.waitForSelector('input[name=title]');
  await owner.type('input[name=title]', '장치 시험');
  await owner.click('form button[type=submit]');
  await sleep(1500);
  bid = Number(owner.url().split('/').pop());
  const restart = async (mode) => {
    if (!(await isIdle(owner))) { await owner.click('.stop-publish'); await waitFor(() => isIdle(owner), 8000); }
    await sleep(1500);
    await owner.evaluate((m) => { Object.assign(window.__m, { audio: 'notfound', video: 'ok', cams: 2, perm: null, delay: 0 }, m); window.__calls = []; }, mode);
    await owner.click('.start-camera');
    return waitFor(() => isLive(owner), 15000);
  };

  // 1) 카메라 방송: 마이크가 없어도 영상만으로 방송 중이 된다
  await owner.waitForSelector('.start-camera');
  check('flip hidden before start', await owner.$eval('.flip-camera', (b) => b.hidden));
  await owner.click('.start-camera');
  check('camera without mic goes live', await waitFor(() => isLive(owner), 15000), await text(owner, '.studio .hint[role=status]'));
  const w1 = await text(owner, '.studio .warn');
  check('mic warning says stop and restart (#68 R6)', w1.includes('마이크를 쓸 수 없어') && w1.includes('송출을 멈추고 다시 시작') && !w1.includes('()'), w1);
  check('retried video, then audio once', await owner.evaluate(() => window.__calls.join(',') === 'video+audio,video,audio'), await owner.evaluate(() => window.__calls.join(',')));
  check('flip visible and enabled when live (2 cameras)', await waitFor(() => owner.$eval('.flip-camera', (b) => !b.hidden && !b.disabled), 3000));

  // 2) 시청: 세로 영상이면 칸이 세로로 길다
  const viewer = await (await browser.createBrowserContext()).newPage();
  await viewer.setViewport({ width: 1280, height: 900 });
  await viewer.goto(`${BASE}/#/b/${bid}`, { waitUntil: 'networkidle2' });
  check('viewer plays', await playing(viewer, 20000));
  const box = await viewer.$eval('.player-box', (b) => { const r = b.getBoundingClientRect(); return { w: r.width, h: r.height, fitted: b.classList.contains('fitted'), vh: innerHeight }; });
  check('portrait box is taller than wide', box.fitted && box.h > box.w * 1.5, JSON.stringify(box));
  check('portrait box fits in 75% of window height', box.h <= box.vh * 0.75 + 2, JSON.stringify(box));
  const op = await viewer.$eval('.op-wrapper', (w) => w.getBoundingClientRect().height).catch(() => -1);
  check('player fills the portrait box', Math.abs(op - box.h) < 3, JSON.stringify({ op, box }));

  // 3) 송출 중 카메라 전환: 끊지 않고 영상 트랙만 바뀐다. 방향을 모르는 카메라는 거울 여부를 바꾸지 않는다(#68 R3)
  const before = await owner.$eval('.preview', (v) => { window.__old = v.srcObject?.getVideoTracks()[0]; return { id: window.__old?.id, mirror: v.classList.contains('mirror') }; });
  await owner.click('.flip-camera');
  check('preview track replaced', await waitFor(async () => (await owner.$eval('.preview', (v) => v.srcObject?.getVideoTracks()[0]?.id)) !== before.id, 5000));
  check('still live after flip', await isLive(owner));
  check('old track stopped', await owner.evaluate(() => window.__old?.readyState === 'ended'));
  check('switched to the other device', await owner.$eval('.preview', (v) => v.srcObject?.getVideoTracks()[0]?.getSettings().deviceId) !== await owner.evaluate(() => window.__old.getSettings().deviceId));
  check('facing select unchanged for camera without facing info', (await owner.$eval('.facing', (s) => s.value)) === 'user');
  check('mirror unchanged for camera without facing info', (await owner.$eval('.preview', (v) => v.classList.contains('mirror'))) === before.mirror);
  check('flip enabled again', await waitFor(() => owner.$eval('.flip-camera', (b) => !b.disabled), 3000));
  check('viewer still plays after flip', await playing(viewer, 10000));

  // 4) 전환 실패(새 카메라도, 원래 카메라도 못 켬) → 이유와 함께 송출을 멈춘다
  await owner.evaluate(() => { window.__m.video = 'fail'; });
  await owner.click('.flip-camera');
  check('flip failure stops publishing', await waitFor(() => isIdle(owner), 8000));
  check('stop reason shown', (await text(owner, '.studio .hint[role=status]')).includes('카메라를 켜지 못해'), await text(owner, '.studio .hint[role=status]'));
  check('flip hidden after stop', await owner.$eval('.flip-camera', (b) => b.hidden));
  check('no device left on after failure', (await liveTracks(owner)) === 0, String(await liveTracks(owner)));

  // 5) 전환 중 멈춤: 새 카메라가 늦게 실패해도 원래 카메라를 다시 켜지 않는다(#68 R4)
  check('restart live', await restart({}));
  await owner.evaluate(() => { window.__m.delay = 1500; window.__m.video = 'fail'; window.__calls = []; });
  await owner.click('.flip-camera');
  await sleep(300);
  await owner.click('.stop-publish');
  await sleep(3000);
  check('no fallback camera after stop', await owner.evaluate(() => window.__calls.join(',') === 'video'), await owner.evaluate(() => window.__calls.join(',')));
  check('idle after stop during flip', await isIdle(owner));

  // 6) 카메라가 1개면 전환 버튼이 없다(#68 R3)
  check('one camera live', await restart({ cams: 1 }));
  await sleep(1000);
  check('flip hidden with one camera', await owner.$eval('.flip-camera', (b) => b.hidden));

  // 7) 마이크만 차단, 카메라는 아직 묻지 않음 → 영상만으로 방송 중(#68 R1)
  check('mic denied + camera prompt goes live', await restart({ audio: 'notallowed', perm: { camera: 'prompt', microphone: 'denied' } }), await text(owner, '.studio .hint[role=status]'));
  check('mic denied warning', (await text(owner, '.studio .warn')).includes('권한이 거부'), await text(owner, '.studio .warn'));
  // 권한 창을 닫은 경우(둘 다 prompt)는 다시 묻지 않는다
  await owner.click('.stop-publish'); await waitFor(() => isIdle(owner), 8000); await sleep(1500);
  await owner.evaluate(() => { Object.assign(window.__m, { audio: 'notallowed', perm: { camera: 'prompt', microphone: 'prompt' } }); window.__calls = []; });
  await owner.click('.start-camera');
  await sleep(2000);
  check('dismissed prompt is not asked again', (await isIdle(owner)) && (await owner.evaluate(() => window.__calls.length === 1)), await owner.evaluate(() => window.__calls.join(',')));

  // 7-2) 카메라는 허용, 마이크 권한 창을 닫음 → 영상만, 마이크는 다시 묻지 않는다(2바퀴 리뷰 1)
  check('cam granted + mic prompt goes live', await restart({ audio: 'notallowed', perm: { camera: 'granted', microphone: 'prompt' } }));
  check('mic not asked again after dismiss', await owner.evaluate(() => window.__calls.join(',') === 'video+audio,video'), await owner.evaluate(() => window.__calls.join(',')));
  check('denied wording says allow', (await text(owner, '.studio .warn')).includes('자물쇠에서 허용'), await text(owner, '.studio .warn'));

  // 7-3) 카메라 3개(방향 정보 없음): 전환하면 모두 돌아가며 고른다(2바퀴 동작 사냥 4)
  check('three cameras live', await restart({ cams: 3 }));
  check('no-mic wording does not say allow', !(await text(owner, '.studio .warn')).includes('허용'), await text(owner, '.studio .warn'));
  await waitFor(() => owner.$eval('.flip-camera', (b) => !b.hidden && !b.disabled), 3000);
  const seen = [];
  for (let i = 0; i < 3; i++) {
    await owner.click('.flip-camera');
    await sleep(800);
    await waitFor(() => owner.$eval('.flip-camera', (b) => !b.disabled), 3000);
    seen.push(await owner.$eval('.preview', (v) => v.srcObject?.getVideoTracks()[0]?.getSettings().deviceId));
  }
  check('cycles through all three cameras', seen.join(',') === 'cam1,cam2,cam0', seen.join(','));

  // 8) 카메라 쪽 잠깐 오류(NotReadable)면 마이크를 버리지 않는다(#68 R2)
  check('transient error goes live', await restart({ audio: 'readable-once' }));
  check('no mic warning after transient error', !(await text(owner, '.studio .warn')).includes('마이크'), await text(owner, '.studio .warn'));
  check('audio track kept', await owner.evaluate(() => window.__live.some((t) => t.kind === 'audio' && t.readyState === 'live')));

  // 9) 화면 공유 + 마이크 체크인데 마이크가 없음 → 화면만으로 방송 중
  await owner.click('.stop-publish'); await waitFor(() => isIdle(owner), 8000); await sleep(1500);
  await owner.evaluate(() => { window.__m.audio = 'notfound'; });
  await owner.click('.start-screen');
  check('screen without mic goes live', await waitFor(() => isLive(owner), 15000), await text(owner, '.studio .hint[role=status]'));
  check('screen mic warning shown', (await text(owner, '.studio .warn')).includes('마이크를 쓸 수 없어'), await text(owner, '.studio .warn'));
  check('flip hidden for screen share', await owner.$eval('.flip-camera', (b) => b.hidden));
  await viewer.reload({ waitUntil: 'networkidle2' });
  check('viewer plays screen', await playing(viewer, 20000));
  const box2 = await viewer.$eval('.player-box', (b) => { const r = b.getBoundingClientRect(); return { w: r.width, h: r.height, fitted: b.classList.contains('fitted') }; });
  check('landscape box is wider than tall', box2.fitted && box2.w > box2.h, JSON.stringify(box2));
  await owner.click('.stop-publish');
  await sleep(2000);
} catch (e) {
  check('no exception', false, e.stack);
} finally {
  if (owner && bid) await owner.evaluate(async (id) => fetch(`/api/broadcasts/${id}`, { method: 'DELETE' }), bid).catch(() => {});
}
console.log(results.join('\n'));
await browser.close();
