// #10 시험: 브라우저에서 WHIP로 OvenMediaEngine에 송출하고, OvenPlayer로 시청한다.
// 확인하려는 것:
//  1) PC 화면 공유 + 탭·시스템 소리 + 마이크를 함께 보낼 수 있는가
//  2) 휴대폰 카메라 + 마이크를 보낼 수 있는가
//  3) WebRTC 시청이 되는가, 안 되면 LL-HLS
//  4) OME가 송출 열림·닫힘을 백엔드에 알려 주는가 (AdmissionWebhooks)

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const logBox = $('log');
const log = (msg: string) => {
  logBox.textContent = `${new Date().toLocaleTimeString()} ${msg}\n` + logBox.textContent;
};

// OME는 이 페이지를 연 주소의 호스트에서 3333 포트로 돈다고 가정한다(로컬 시험).
const omeHost = location.hostname;
const omeHttp = `${location.protocol}//${omeHost}:3333`;
const omeWs = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${omeHost}:3333`;
const streamName = () => ($('stream') as HTMLInputElement).value.trim() || 'test1';

let pc: RTCPeerConnection | null = null;
let tracks: MediaStreamTrack[] = [];
let audioCtx: AudioContext | null = null;
let whipResource: string | null = null;

function setState(s: string) {
  $('state').textContent = s;
}

/// 소리 트랙 여러 개를 하나로 섞는다.
/// 이유: 마이크 소리와 탭 소리를 따로 보내면 서버가 둘 다 받는지 확실하지 않다.
/// 브라우저 안에서 하나로 섞어 보내면 서버 쪽 지원과 상관없이 시청자가 둘 다 듣는다.
function mixAudio(audioTracks: MediaStreamTrack[]): MediaStreamTrack | null {
  if (audioTracks.length === 0) return null;
  if (audioTracks.length === 1) return audioTracks[0];
  audioCtx = new AudioContext();
  const dest = audioCtx.createMediaStreamDestination();
  for (const t of audioTracks) {
    audioCtx.createMediaStreamSource(new MediaStream([t])).connect(dest);
  }
  return dest.stream.getAudioTracks()[0];
}

async function publish(video: MediaStreamTrack, audio: MediaStreamTrack | null) {
  pc = new RTCPeerConnection();
  const vt = pc.addTransceiver(video, { direction: 'sendonly' });
  // 영상 형식은 H.264를 먼저 쓴다. HLS(LL-HLS)는 H.264만 담을 수 있어서,
  // 브라우저 기본(VP8)으로 보내면 LL-HLS 시청자에게 화면이 안 간다. 서버에서 형식 변환(무거움)을 안 하려는 것이다.
  const h264 = RTCRtpSender.getCapabilities('video')?.codecs.filter((c) => c.mimeType === 'video/H264') ?? [];
  if (h264.length > 0 && 'setCodecPreferences' in vt) {
    const rest = RTCRtpSender.getCapabilities('video')!.codecs.filter((c) => c.mimeType !== 'video/H264');
    vt.setCodecPreferences([...h264, ...rest]);
    log(`영상 형식: H.264 우선 (후보 ${h264.length}개)`);
  } else {
    log('영상 형식: 이 브라우저는 H.264 우선 설정을 못 함 → 기본 형식으로 송출');
  }
  if (audio) pc.addTransceiver(audio, { direction: 'sendonly' });
  pc.onconnectionstatechange = () => {
    log(`연결 상태: ${pc?.connectionState}`);
    setState(pc?.connectionState ?? '');
  };

  const offer = await pc.createOffer();
  await pc.setLocalDescription(offer);

  const url = `${omeHttp}/app/${encodeURIComponent(streamName())}?direction=whip`;
  log(`WHIP 요청: ${url}`);
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/sdp' },
    body: offer.sdp,
  });
  if (!res.ok) {
    throw new Error(`WHIP 실패: ${res.status} ${await res.text()}`);
  }
  whipResource = res.headers.get('Location');
  await pc.setRemoteDescription({ type: 'answer', sdp: await res.text() });
  log(`WHIP 응답 받음. 자원 주소: ${whipResource ?? '(없음)'}`);
}

async function startPc() {
  // 화면 공유: 크롬은 공유 창에서 "탭 오디오 공유"·"시스템 오디오 공유"를 고를 수 있다.
  const screen = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: true });
  const mic = await navigator.mediaDevices.getUserMedia({ audio: true });
  const screenAudio = screen.getAudioTracks();
  log(`화면 소리 트랙 ${screenAudio.length}개, 마이크 트랙 ${mic.getAudioTracks().length}개`);
  if (screenAudio.length === 0) log('주의: 화면 공유 창에서 탭/시스템 오디오를 체크하지 않았거나, 이 브라우저가 지원하지 않음');
  const video = screen.getVideoTracks()[0];
  // 공유 창의 "공유 중지"를 누르면 송출도 끝낸다.
  video.onended = () => stop();
  const audio = mixAudio([...screenAudio, ...mic.getAudioTracks()]);
  tracks = [...screen.getTracks(), ...mic.getTracks()];
  ($('preview') as HTMLVideoElement).srcObject = new MediaStream([video]);
  await publish(video, audio);
}

async function startMobile() {
  const cam = await navigator.mediaDevices.getUserMedia({
    video: { facingMode: 'user' },
    audio: true,
  });
  tracks = cam.getTracks();
  ($('preview') as HTMLVideoElement).srcObject = cam;
  await publish(cam.getVideoTracks()[0], cam.getAudioTracks()[0] ?? null);
}

async function stop() {
  // WHIP 규칙: 송출을 끝낼 때 자원 주소에 DELETE를 보낸다.
  if (whipResource) {
    const u = new URL(whipResource, omeHttp);
    await fetch(u, { method: 'DELETE' }).catch(() => undefined);
    log(`WHIP DELETE: ${u}`);
  }
  pc?.close();
  tracks.forEach((t) => t.stop());
  await audioCtx?.close().catch(() => undefined);
  pc = null;
  tracks = [];
  audioCtx = null;
  whipResource = null;
  setState('종료');
  log('송출 종료');
}

declare const OvenPlayer: {
  create: (id: string, opts: unknown) => { remove: () => void; on: (e: string, cb: (d: unknown) => void) => void };
  getPlayerByContainerId: (id: string) => { remove: () => void } | null;
};

function watch(kind: 'webrtc' | 'llhls') {
  OvenPlayer.getPlayerByContainerId('player')?.remove();
  const name = encodeURIComponent(streamName());
  const source =
    kind === 'webrtc'
      ? { type: 'webrtc', file: `${omeWs}/app/${name}` }
      : { type: 'll-hls', file: `${omeHttp}/app/${name}/llhls.m3u8` };
  const player = OvenPlayer.create('player', { autoStart: true, mute: true, sources: [source] });
  player.on('error', (e) => log(`플레이어 오류: ${JSON.stringify(e)}`));
  log(`시청 시작: ${source.file}`);
}

async function pollEvents() {
  try {
    const res = await fetch('/api/ome/events');
    const list: { time: string; direction: string; protocol: string; status: string; url: string }[] = await res.json();
    $('events').textContent = list
      .slice()
      .reverse()
      .map((e) => `${e.time} ${e.direction} ${e.protocol} ${e.status} ${e.url}`)
      .join('\n');
  } catch {
    $('events').textContent = '백엔드(/api/ome/events)에 연결하지 못함';
  }
}

const guard = (fn: () => Promise<void>) => () =>
  fn().catch((e: unknown) => {
    log(`오류: ${e instanceof Error ? e.message : String(e)}`);
    setState('오류');
  });

$('pc').onclick = guard(startPc);
$('mobile').onclick = guard(startMobile);
$('stop').onclick = guard(stop);
$('watch-webrtc').onclick = () => watch('webrtc');
$('watch-llhls').onclick = () => watch('llhls');
setInterval(pollEvents, 2000);
pollEvents();
