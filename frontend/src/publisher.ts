// 브라우저에서 WHIP로 OME에 송출한다(#1 PC 화면 공유, #2 휴대폰 카메라에서 함께 쓴다).
//
// 구조: 시작할 때마다 "시도(Attempt)"를 하나 만든다. 시도는 자기가 켠 장치·연결·WHIP 자원을 스스로 들고 있고,
// 멈추거나 실패하면 자기 것만 정리한다(#30 P1·P2).
//  - 멈춘 뒤에 늦게 끝난 단계(장치 켜짐, 송출 요청 응답)가 와도, 그 시도가 이미 취소됐으면 바로 정리한다.
//    특히 송출 요청이 성공한 뒤에 취소됐으면 그 WHIP 자원에도 DELETE를 보낸다(OME 세션이 남아 409가 나던 문제).
//  - 정리는 필드를 먼저 비우고 지역 변수로 정리해서, 여러 번·겹쳐서 불러도 남의 시도를 건드리지 않는다.
//  - 한 번에 하나만 송출한다(두 번 시작하면 OME가 같은 이름이라 409).
//  - 소리 여러 개는 브라우저에서 하나로 섞어 보낸다(시청 플레이어가 소리 트랙 여러 개를 함께 트는지 확실하지 않아서).

export type PublishState = 'idle' | 'starting' | 'live' | 'stopping';

/** 사용자에게 보여 줄 수 있는 송출 오류 */
export class PublishError extends Error {}
/** 멈춰서 취소된 시도. 화면에는 보여 주지 않는다(#30 P7). */
export class CancelledError extends Error {
  constructor() {
    super('취소했습니다');
  }
}

/** 시작 한 번의 자원들. 취소되면 이후에 생기는 자원도 바로 정리한다. */
class Attempt {
  cancelled = false;
  tracks: MediaStreamTrack[] = [];
  pc: RTCPeerConnection | null = null;
  resource: string | null = null;
  audioCtx: AudioContext | null = null;

  /** 켠 장치를 맡긴다. 이미 취소됐으면 바로 끈다. */
  add(t: MediaStreamTrack[]) {
    if (this.cancelled) t.forEach((x) => x.stop());
    else this.tracks.push(...t);
  }

  /** 취소됐으면 더 진행하지 않는다 */
  check() {
    if (this.cancelled) throw new CancelledError();
  }

  /** 들고 있는 것을 모두 정리한다. 여러 번 불러도 된다(부를 때 들고 있는 것만 정리). */
  async close() {
    const { tracks, pc, resource, audioCtx } = this;
    this.tracks = [];
    this.pc = null;
    this.resource = null;
    this.audioCtx = null;
    // 장치와 연결부터 끈다(개인정보: 화면 공유·마이크를 먼저 끔)
    for (const t of tracks) {
      t.onended = null;
      t.stop();
    }
    pc?.close();
    // WHIP 규칙: 끝낼 때 자원 주소에 DELETE. OME가 세션을 바로 지워 다시 시작할 때 409가 나지 않는다.
    // 다른 정리(소리 섞개 닫기)를 기다리기 전에 먼저 보낸다. keepalive라 그 사이 페이지가 사라져도 끝까지 간다(#31 리뷰:
    // 휴대폰은 화면이 숨겨질 때 멈춤이 먼저 시작되고 곧바로 페이지가 사라질 수 있다).
    const del = resource ? fetch(resource, { method: 'DELETE', keepalive: true }).catch(() => {}) : null;
    await audioCtx?.close().catch(() => {});
    await del;
  }

  /**
   * 페이지를 닫거나 새로고침할 때 쓴다(#31 P14). 기다리지 않고 바로 끝낸다.
   * keepalive로 보낸 요청은 페이지가 사라져도 브라우저가 끝까지 보낸다. 그래서 OME가 연결 끊김을 알아챌 때까지
   * "방송 중"이 남지 않고, 곧바로 다시 송출해도 409(이미 송출 중)가 나지 않는다.
   */
  abandon() {
    this.cancelled = true;
    const { tracks, pc, resource } = this;
    this.tracks = [];
    this.pc = null;
    this.resource = null;
    for (const t of tracks) {
      t.onended = null;
      t.stop();
    }
    pc?.close();
    if (resource) void fetch(resource, { method: 'DELETE', keepalive: true }).catch(() => {});
  }
}

export class Publisher {
  state: PublishState = 'idle';
  /** 상태가 바뀔 때마다 부른다(화면 갱신용). detail은 멈춘 이유나 실패 이유. */
  onState: (s: PublishState, detail?: string) => void = () => {};

  private current: Attempt | null = null;

  /** 페이지를 떠날 때(pagehide) 부른다. 송출 중이면 WHIP 자원에 DELETE를 보내고 장치를 끈다(#31 P14). */
  abandon() {
    const a = this.current;
    this.current = null;
    a?.abandon();
    // 뒤로 가기 캐시로 페이지가 되살아나도 "송출 중"으로 남지 않게 대기로 돌린다(#31 리뷰)
    if (this.state !== 'idle') this.set('idle');
  }

  private set(s: PublishState, detail?: string) {
    this.state = s;
    this.onState(s, detail);
  }

  /**
   * 송출을 시작한다. 버튼 클릭 안에서 곧바로(기다리지 않고) 불러야 한다.
   * 장치 켜기(공유 창)를 먼저 하고, 송출 주소는 그다음에 받는다. 공유 창은 사용자 동작 직후에만 열 수 있다(#30 P4).
   * @param getTracks 장치를 켜서 트랙을 돌려준다. `register`로 켠 트랙을 바로 알려 주면 다음 단계에서 실패해도 정리된다.
   * @param whipUrl  송출 주소(같은 사이트의 /app/... 경로)를 돌려주는 함수
   */
  async start(
    getTracks: (register: (t: MediaStreamTrack[]) => void) => Promise<{ video: MediaStreamTrack; audio: MediaStreamTrack[] }>,
    whipUrl: () => Promise<string>,
    /** 영상 트랙이 끝났을 때 보여 줄 이유(화면 공유 끝남, 카메라 꺼짐 등) */
    endedReason = '화면 공유가 끝났습니다',
  ): Promise<MediaStream> {
    if (this.state !== 'idle') throw new PublishError('이미 송출 중입니다');
    const a = new Attempt();
    this.current = a;
    this.set('starting');
    try {
      // 소리 섞기용 AudioContext는 클릭 직후에 만든다. 사파리는 그래야 소리가 난다.
      // 만들 수 없는 브라우저(옛 사파리 등)면 섞지 않고 첫 소리만 보낸다(#30 2바퀴).
      try {
        a.audioCtx = new AudioContext();
      } catch {
        a.audioCtx = null;
      }
      const { video, audio } = await getTracks((t) => a.add(t));
      a.check();
      // 마이크 권한 창이 떠 있는 사이에 화면 공유를 끝냈으면, 이미 끝난 트랙에는 ended가 다시 오지 않는다(#30 2바퀴)
      if (video.readyState === 'ended') throw new PublishError(endedReason);
      // 준비 중에 공유 창의 "공유 중지"를 눌러도 멈춘다(#30 P11)
      // (휴대폰에서 다른 앱으로 가서 카메라가 꺼진 경우도 같다, #2)
      video.onended = () => {
        if (this.current === a) void this.stop(endedReason);
      };
      await a.audioCtx?.resume().catch(() => {});
      const mixed = mix(a, audio);
      const url = await whipUrl();
      a.check();

      const pc = new RTCPeerConnection();
      a.pc = pc;
      const vt = pc.addTransceiver(video, { direction: 'sendonly' });
      preferH264(vt);
      if (mixed) pc.addTransceiver(mixed, { direction: 'sendonly' });
      pc.onconnectionstatechange = () => {
        if (this.current !== a) return;
        if (pc.connectionState === 'connected' && this.state === 'starting') this.set('live');
        if (pc.connectionState === 'failed') void this.stop('연결이 끊겼습니다');
      };

      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      a.check();
      const res = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/sdp' },
        body: offer.sdp,
      }).catch(() => {
        throw new PublishError('송출 서버에 연결하지 못했습니다');
      });
      // 취소 여부를 보기 전에 자원 주소부터 받아 둔다. 취소됐어도 close()가 DELETE를 보낼 수 있게(#30 P1).
      if (res.ok) a.resource = sameOriginPath(res.headers.get('Location'));
      a.check();
      if (!res.ok) {
        throw new PublishError(
          res.status === 401 || res.status === 403
            ? '송출이 거절됐습니다(종료된 방송이거나 키가 맞지 않습니다)'
            : res.status === 409
              ? '이미 이 방송으로 송출 중인 곳이 있습니다. 잠시 뒤 다시 시도하세요'
              : `송출 서버 오류 (${res.status})`,
        );
      }
      await pc.setRemoteDescription({ type: 'answer', sdp: await res.text() });
      a.check();
      return new MediaStream([video]);
    } catch (e) {
      // 이 시도가 들고 있던 것을 정리한다(이미 멈춰서 정리됐더라도, 그 뒤에 생긴 자원까지)
      await a.close();
      // 멈춰서 취소된 시도에서 난 오류(닫힌 연결에 대한 InvalidStateError 등)는 모두 취소로 본다.
      // 그래야 멈춘 이유를 영어 오류가 덮지 않는다(#30 2바퀴).
      if (e instanceof CancelledError || a.cancelled) throw new CancelledError();
      const err = e instanceof PublishError ? e : new PublishError(message(e));
      // 다른 시도가 이미 시작됐으면 상태는 그쪽이 맡는다
      if (this.current === a) {
        this.current = null;
        this.set('idle', err.message);
      }
      throw err;
    }
  }

  /**
   * 송출을 끊지 않고 영상 트랙만 바꾼다(송출 중 카메라 전환, #67).
   * 옛 영상을 먼저 끄고 새 영상을 받는다. 휴대폰은 카메라 두 개를 동시에 켜지 못하는 경우가 많다.
   * @param get 새 영상 트랙을 켠다. 실패하면 던진다(옛 카메라로 되돌리는 것은 부르는 쪽이 get 안에서 한다).
   * @returns 새 영상 트랙. get이 실패하면 송출을 멈추고 PublishError를 던진다.
   */
  async switchVideo(get: () => Promise<MediaStreamTrack>, endedReason: string): Promise<MediaStreamTrack> {
    const a = this.current;
    const sender = a?.pc?.getSenders().find((s) => s.track?.kind === 'video');
    if (!a || this.state !== 'live' || !sender) throw new PublishError('송출 중일 때만 바꿀 수 있습니다');
    const old = sender.track;
    if (old) {
      old.onended = null;
      old.stop();
      a.tracks = a.tracks.filter((t) => t !== old);
    }
    let next: MediaStreamTrack;
    try {
      next = await get();
    } catch (e) {
      // 영상 없이 소리만 가는 방송이 되지 않게 멈춘다
      if (this.current === a) await this.stop(`카메라를 켜지 못해 송출을 멈췄습니다(${message(e)})`);
      throw e instanceof CancelledError ? e : new PublishError(message(e));
    }
    // 카메라를 켜는 사이에 멈췄으면 새 카메라도 끈다
    if (this.current !== a || a.cancelled) {
      next.stop();
      throw new CancelledError();
    }
    a.add([next]);
    next.onended = () => {
      if (this.current === a) void this.stop(endedReason);
    };
    try {
      await sender.replaceTrack(next);
    } catch (e) {
      if (this.current === a) await this.stop(`카메라를 바꾸지 못해 송출을 멈췄습니다(${message(e)})`);
      throw new PublishError(message(e));
    }
    return next;
  }

  /** 송출을 멈춘다. 몇 번 불러도 되고, 겹쳐 불러도 된다. */
  async stop(reason?: string) {
    const a = this.current;
    if (!a) return;
    this.current = null;
    a.cancelled = true;
    this.set('stopping');
    await a.close();
    // 정리하는 사이 새 시도가 시작됐으면 상태를 덮어쓰지 않는다
    if (this.current === null) this.set('idle', reason);
  }
}

/** 소리가 여러 개면 하나로 섞는다. 섞은 결과 트랙도 시도가 들고 있다가 정리한다. */
function mix(a: Attempt, audio: MediaStreamTrack[]): MediaStreamTrack | null {
  if (audio.length === 0) return null;
  if (audio.length === 1 || !a.audioCtx) return audio[0];
  const dest = a.audioCtx.createMediaStreamDestination();
  for (const t of audio) a.audioCtx.createMediaStreamSource(new MediaStream([t])).connect(dest);
  const out = dest.stream.getAudioTracks()[0];
  a.add([out]);
  return out;
}

/** OME가 돌려준 WHIP 자원 주소. 다른 출처(http://...:3333 등)면 https 페이지에서 부를 수 없으니 버린다. */
function sameOriginPath(loc: string | null): string | null {
  if (!loc) return null;
  const u = new URL(loc, location.origin);
  return u.origin === location.origin ? u.pathname + u.search : null;
}

/** 영상은 H.264를 먼저 쓴다. LL-HLS는 H.264만 담아서, VP8로 보내면 LL-HLS 시청자에게 화면이 안 간다(#10). */
function preferH264(t: RTCRtpTransceiver) {
  const caps = RTCRtpSender.getCapabilities?.('video')?.codecs ?? [];
  const h264 = caps.filter((c) => c.mimeType === 'video/H264');
  if (h264.length > 0 && 'setCodecPreferences' in t) {
    t.setCodecPreferences([...h264, ...caps.filter((c) => c.mimeType !== 'video/H264')]);
  }
}

export function message(e: unknown): string {
  if (e instanceof DOMException) {
    if (e.name === 'NotAllowedError') return '권한이 거부됐습니다. 주소창 옆 자물쇠에서 허용한 뒤 다시 시도하세요';
    if (e.name === 'NotFoundError') return '장치(마이크·카메라)를 찾지 못했습니다';
    if (e.name === 'NotReadableError') return '다른 프로그램이 장치를 쓰고 있습니다';
    if (e.name === 'AbortError') return '장치를 켜다가 중단됐습니다. 다시 시도하세요';
  }
  return e instanceof Error ? e.message : String(e);
}

/** PC 화면 공유 + (선택) 마이크 */
export async function screenAndMic(register: (t: MediaStreamTrack[]) => void, withMic: boolean) {
  if (!navigator.mediaDevices?.getDisplayMedia) {
    throw new PublishError('이 브라우저는 화면 공유를 지원하지 않습니다. PC의 Chrome이나 Edge를 쓰세요');
  }
  let screen: MediaStream;
  try {
    // 크롬·엣지: 공유 창에서 "탭 오디오 공유"·"시스템 오디오 공유"를 고를 수 있다. systemAudio는 그 칸을 보여 달라는 힌트다.
    screen = await navigator.mediaDevices.getDisplayMedia({
      video: true,
      audio: true,
      systemAudio: 'include',
    } as DisplayMediaStreamOptions);
  } catch (e) {
    // 공유 창에서 취소해도 NotAllowedError가 온다. 화면 공유는 자물쇠 권한과 상관없어서 따로 알린다(#30 P9).
    if (e instanceof DOMException && e.name === 'NotAllowedError') {
      throw new PublishError('화면 공유를 취소했거나 허용하지 않았습니다');
    }
    throw new PublishError(message(e));
  }
  register(screen.getTracks());
  const audio = [...screen.getAudioTracks()];
  // 마이크가 없거나 막혀도 화면은 보낸다. 이유는 화면에 경고로 보여 준다(#67, #31 P10).
  let micError = '';
  if (withMic) {
    try {
      const mic = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
      register(mic.getTracks());
      audio.push(...mic.getAudioTracks());
    } catch (e) {
      micError = micFailed(e);
    }
  }
  return { video: screen.getVideoTracks()[0], audio, screenAudio: screen.getAudioTracks().length, micError };
}

/** 마이크를 못 켰을 때 보여 줄 문장. 이미 송출 중이라 "다시 시도"가 아니라 멈추고 다시 시작하라고 쓴다(#68 R6). */
function micFailed(e: unknown): string {
  const why = message(e).split('. ')[0] || (e instanceof DOMException ? e.name : '알 수 없는 이유');
  return `마이크를 쓸 수 없어 마이크 소리 없이 송출합니다(${why}). 마이크를 쓰려면 허용한 뒤 송출을 멈추고 다시 시작하세요.`;
}

/** 권한 상태. 알 수 없으면(지원 안 하는 브라우저) 'unknown'. 읽기만 해서 권한 창은 뜨지 않는다. */
async function permission(name: 'camera' | 'microphone'): Promise<PermissionState | 'unknown'> {
  try {
    return (await navigator.permissions.query({ name: name as PermissionName })).state;
  } catch {
    return 'unknown';
  }
}

/** 카메라 영상 조건. 방향은 '바람(ideal)'이라 고른 카메라가 없으면 다른 카메라가 켜진다. */
function cameraVideo(facing: Facing): MediaTrackConstraints {
  return { facingMode: facing, width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } };
}

async function firstVideo(c: MediaTrackConstraints): Promise<MediaStreamTrack> {
  const s = await navigator.mediaDevices.getUserMedia({ video: c });
  const t = s.getVideoTracks()[0];
  if (!t) throw new PublishError('카메라 영상을 받지 못했습니다');
  return t;
}

/** 카메라 개수. 권한을 받은 뒤에 불러야 정확하다. 알 수 없으면 0. */
export async function cameraCount(): Promise<number> {
  try {
    return (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === 'videoinput').length;
  } catch {
    return 0;
  }
}

/**
 * 송출 중 카메라 전환용으로 다른 카메라 영상 트랙 하나만 켠다(#67).
 * 휴대폰은 방향(앞·뒤)으로 고른다. 방향을 모르는 PC 카메라는 방향 요청에 같은 카메라가 다시 켜질 수 있어서,
 * 그때는 다른 카메라를 장치 번호로 켠다(#68 R3).
 * @param fromId 지금까지 쓰던 카메라의 장치 번호(이미 꺼져 있어야 한다)
 */
export async function otherCamera(facing: Facing, fromId?: string): Promise<MediaStreamTrack> {
  const t = await firstVideo(cameraVideo(facing));
  if (!fromId || t.getSettings().deviceId !== fromId) return t;
  const devices = await navigator.mediaDevices.enumerateDevices().catch(() => [] as MediaDeviceInfo[]);
  const other = devices.find((d) => d.kind === 'videoinput' && d.deviceId && d.deviceId !== fromId);
  if (!other) return t;
  t.stop();
  return firstVideo({ deviceId: { exact: other.deviceId }, width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } });
}

/** 원래 카메라를 다시 켠다(전환 실패 시 되돌리기). 장치 번호를 알면 그 카메라를 켠다. */
export async function sameCamera(facing: Facing, id?: string): Promise<MediaStreamTrack> {
  if (id) return firstVideo({ deviceId: { exact: id }, width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } });
  return firstVideo(cameraVideo(facing));
}

export type Facing = 'user' | 'environment';

/** 휴대폰·PC 카메라 + 마이크(#2). 권한 창이 한 번만 뜨도록 영상과 소리를 함께 요청한다. */
export async function cameraAndMic(register: (t: MediaStreamTrack[]) => void, facing: Facing) {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new PublishError('이 브라우저는 카메라를 쓸 수 없습니다. https 주소인지 확인하세요');
  }
  const audioC = { echoCancellation: true, noiseSuppression: true };
  try {
    const cam = await navigator.mediaDevices.getUserMedia({ video: cameraVideo(facing), audio: audioC });
    register(cam.getTracks());
    const video = cam.getVideoTracks()[0];
    if (!video) throw new PublishError('카메라 영상을 받지 못했습니다');
    return { video, audio: cam.getAudioTracks(), micError: '' };
  } catch (e) {
    if (e instanceof PublishError) throw e;
    // facingMode 등은 '바람(ideal)'이라 고른 카메라가 없어도 실패하지 않고 다른 카메라가 켜진다.
    // 실제로 켜진 카메라는 화면 쪽에서 getSettings()로 보고 알린다(#33 M4).
    // 영상과 소리를 함께 요청하면 마이크 하나 때문에 카메라도 못 쓴다. 마이크 탓일 수 있으면 영상만 다시 요청한다(#67, #34 M5).
    // 권한 거부는 다음 경우에만 다시 묻는다. 사용자가 방금 권한 창을 닫았는데 창이 또 뜨지 않게.
    //  - 카메라는 이미 허용됨
    //  - 마이크는 차단됐고 카메라는 차단되지 않음(아직 묻지 않음이면 카메라 권한 창이 뜬다. 카메라 시작을 눌렀으니 예상할 수 있는 창이다, #68 R1)
    const name = e instanceof DOMException ? e.name : '';
    let retry = name === 'NotFoundError' || name === 'NotReadableError' || name === 'OverconstrainedError';
    if (name === 'NotAllowedError') {
      const [cam, mic] = await Promise.all([permission('camera'), permission('microphone')]);
      retry = cam === 'granted' || (mic === 'denied' && cam !== 'denied');
    }
    if (!retry) throw new PublishError(message(e));
    let video: MediaStreamTrack;
    try {
      video = await firstVideo(cameraVideo(facing));
    } catch {
      // 카메라 자체가 안 되면 처음 오류를 보여 준다
      throw new PublishError(message(e));
    }
    register([video]);
    // 처음 실패가 카메라 쪽의 잠깐 오류였을 수도 있다. 마이크만 한 번 더 요청해 보고, 그것도 실패할 때만 마이크 없이 보낸다(#68 R2).
    try {
      const mic = await navigator.mediaDevices.getUserMedia({ audio: audioC });
      register(mic.getTracks());
      return { video, audio: mic.getAudioTracks(), micError: '' };
    } catch (e2) {
      return { video, audio: [] as MediaStreamTrack[], micError: micFailed(e2) };
    }
  }
}

/**
 * 송출하는 동안 화면이 꺼지지 않게 한다(지원하는 브라우저만). 돌려준 함수를 부르면 푼다.
 * 탭을 떠났다 돌아오면 브라우저가 잠금을 풀어 버리므로 다시 건다.
 */
export function keepScreenOn(): () => void {
  type Sentinel = { release: () => Promise<void> };
  const wl = (navigator as unknown as { wakeLock?: { request: (t: 'screen') => Promise<Sentinel> } }).wakeLock;
  if (!wl) return () => {};
  let sentinel: Sentinel | null = null;
  let pending = false;
  let active = true;
  const lock = () => {
    // 이미 잠갔거나 요청 중이면 또 요청하지 않는다(겹치면 하나를 놓쳐 풀지 못한다, #33 M7)
    if (!active || pending || sentinel || document.visibilityState !== 'visible') return;
    pending = true;
    wl.request('screen')
      .then((s) => {
        if (!active) {
          void s.release();
          return;
        }
        sentinel = s;
        // 탭을 떠나면 브라우저가 잠금을 푼다. 그때 비워 둬야 돌아왔을 때 다시 잠근다.
        (s as unknown as EventTarget).addEventListener?.('release', () => {
          if (sentinel === s) sentinel = null;
        });
      })
      .catch(() => {})
      .finally(() => {
        pending = false;
      });
  };
  document.addEventListener('visibilitychange', lock);
  lock();
  return () => {
    active = false;
    document.removeEventListener('visibilitychange', lock);
    void sentinel?.release().catch(() => {});
    sentinel = null;
  };
}
