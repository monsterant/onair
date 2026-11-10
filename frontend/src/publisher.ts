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
    await audioCtx?.close().catch(() => {});
    // WHIP 규칙: 끝낼 때 자원 주소에 DELETE. OME가 세션을 바로 지워 다시 시작할 때 409가 나지 않는다.
    if (resource) await fetch(resource, { method: 'DELETE' }).catch(() => {});
  }
}

export class Publisher {
  state: PublishState = 'idle';
  /** 상태가 바뀔 때마다 부른다(화면 갱신용). detail은 멈춘 이유나 실패 이유. */
  onState: (s: PublishState, detail?: string) => void = () => {};

  private current: Attempt | null = null;

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
      if (video.readyState === 'ended') throw new PublishError('화면 공유가 끝났습니다');
      // 준비 중에 공유 창의 "공유 중지"를 눌러도 멈춘다(#30 P11)
      video.onended = () => {
        if (this.current === a) void this.stop('화면 공유가 끝났습니다');
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
  if (withMic) {
    try {
      const mic = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
      register(mic.getTracks());
      audio.push(...mic.getAudioTracks());
    } catch (e) {
      throw new PublishError(
        `${message(e)}. 마이크 없이 하려면 "마이크 소리도 함께 보내기"를 끄고 다시 시작하세요`,
      );
    }
  }
  return { video: screen.getVideoTracks()[0], audio, screenAudio: screen.getAudioTracks().length };
}
