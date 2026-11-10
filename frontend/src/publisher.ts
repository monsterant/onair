// 브라우저에서 WHIP로 OME에 송출한다(#1 PC 화면 공유, #2 휴대폰 카메라에서 함께 쓴다).
// #10 시험(spike.ts)에서 확인한 문제를 여기서 막는다:
//  - 어느 단계에서 실패해도 켜 둔 장치(화면 공유·마이크·카메라)와 연결을 모두 정리한다.
//  - 한 번에 하나만 송출한다(두 번 시작하면 OME가 같은 이름이라 409).
//  - 준비 중에도 멈출 수 있다. 연결은 이 객체 안에 들고 있다.
//  - 소리 여러 개는 브라우저에서 하나로 섞어 보낸다(시청 플레이어가 소리 트랙 여러 개를 함께 트는지 확실하지 않아서).

export type PublishState = 'idle' | 'starting' | 'live' | 'stopping';

export class PublishError extends Error {}

export class Publisher {
  state: PublishState = 'idle';
  /** 상태가 바뀔 때마다 부른다(화면 갱신용) */
  onState: (s: PublishState, detail?: string) => void = () => {};

  private pc: RTCPeerConnection | null = null;
  private tracks: MediaStreamTrack[] = [];
  private audioCtx: AudioContext | null = null;
  private resource: string | null = null;
  /** 시작을 시도할 때마다 1씩 는다. 준비 중에 멈췄으면 늦게 끝난 준비 결과를 버린다. */
  private attempt = 0;

  private set(s: PublishState, detail?: string) {
    this.state = s;
    this.onState(s, detail);
  }

  /**
   * 송출을 시작한다.
   * @param getTracks 장치를 켜서 트랙을 돌려주는 함수(화면 공유, 카메라 등). 실패하면 던진다.
   *                  `register`로 켠 트랙을 바로 알려 주면, 다음 단계에서 실패해도 정리된다.
   * @param whipUrl  송출 주소(같은 사이트의 /app/... 경로)
   */
  async start(
    getTracks: (register: (t: MediaStreamTrack[]) => void) => Promise<{ video: MediaStreamTrack; audio: MediaStreamTrack[] }>,
    whipUrl: string,
  ): Promise<MediaStream> {
    if (this.state !== 'idle') throw new PublishError('이미 송출 중입니다');
    const my = ++this.attempt;
    this.set('starting');
    // 소리 섞기용 AudioContext는 버튼을 누른 직후(사용자 동작 안에서) 만든다. 사파리는 그래야 소리가 난다.
    this.audioCtx = new AudioContext();
    try {
      // 장치가 켜지는 사이에 멈췄으면(시도 번호가 바뀜) 늦게 켜진 장치는 바로 끈다
      const register = (t: MediaStreamTrack[]) => {
        if (my === this.attempt) this.tracks.push(...t);
        else t.forEach((x) => x.stop());
      };
      const { video, audio } = await getTracks(register);
      if (my !== this.attempt) throw new PublishError('취소했습니다');
      await this.audioCtx.resume().catch(() => {});
      const mixed = this.mix(audio);

      const pc = new RTCPeerConnection();
      this.pc = pc;
      const vt = pc.addTransceiver(video, { direction: 'sendonly' });
      preferH264(vt);
      if (mixed) pc.addTransceiver(mixed, { direction: 'sendonly' });
      pc.onconnectionstatechange = () => {
        if (this.pc !== pc) return;
        if (pc.connectionState === 'connected' && this.state === 'starting') this.set('live');
        if (pc.connectionState === 'failed') this.stop('연결이 끊겼습니다');
      };

      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      const res = await fetch(whipUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/sdp' },
        body: offer.sdp,
      });
      if (my !== this.attempt) throw new PublishError('취소했습니다');
      if (!res.ok) {
        throw new PublishError(
          res.status === 401 || res.status === 403
            ? '송출이 거절됐습니다(종료된 방송이거나 키가 맞지 않습니다)'
            : res.status === 409
              ? '이미 이 방송으로 송출 중인 곳이 있습니다'
              : `송출 서버 오류 (${res.status})`,
        );
      }
      const loc = res.headers.get('Location');
      // OME가 돌려주는 자원 주소. 다른 출처(http://...:3333)면 https 페이지에서 부를 수 없으니 같은 출처 경로만 쓴다.
      if (loc) {
        const u = new URL(loc, location.origin);
        this.resource = u.origin === location.origin ? u.pathname + u.search : null;
      }
      await pc.setRemoteDescription({ type: 'answer', sdp: await res.text() });
      const preview = new MediaStream([video]);
      // 공유 창의 "공유 중지"를 누르면 송출도 멈춘다
      video.onended = () => this.stop('화면 공유가 끝났습니다');
      return preview;
    } catch (e) {
      const err = e instanceof PublishError ? e : new PublishError(message(e));
      // 이번 시도가 아직 유효하면(중간에 멈추지 않았으면) 모두 정리한다.
      // 이미 멈췄거나 새 시도가 시작됐으면 그쪽이 상태를 맡으므로 건드리지 않는다.
      if (my === this.attempt) {
        await this.cleanup();
        this.set('idle', err.message);
      }
      throw err;
    }
  }

  /** 송출을 멈춘다. 몇 번 불러도 된다. */
  async stop(reason?: string) {
    if (this.state === 'idle') return;
    this.attempt++; // 준비 중이었다면 그 결과를 버린다
    this.set('stopping');
    await this.cleanup();
    this.set('idle', reason);
  }

  private async cleanup() {
    const pc = this.pc;
    const resource = this.resource;
    this.pc = null;
    this.resource = null;
    // WHIP 규칙: 끝낼 때 자원 주소에 DELETE. 실패해도 연결을 닫으면 OME가 곧 알아챈다.
    if (resource) await fetch(resource, { method: 'DELETE' }).catch(() => {});
    pc?.close();
    for (const t of this.tracks) {
      t.onended = null;
      t.stop();
    }
    this.tracks = [];
    await this.audioCtx?.close().catch(() => {});
    this.audioCtx = null;
  }

  private mix(audio: MediaStreamTrack[]): MediaStreamTrack | null {
    if (audio.length === 0) return null;
    if (audio.length === 1 || !this.audioCtx) return audio[0];
    const dest = this.audioCtx.createMediaStreamDestination();
    for (const t of audio) this.audioCtx.createMediaStreamSource(new MediaStream([t])).connect(dest);
    const out = dest.stream.getAudioTracks()[0];
    this.tracks.push(out);
    return out;
  }
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
  }
  return e instanceof Error ? e.message : String(e);
}

/** PC 화면 공유 + 마이크. 마이크가 없거나 거부돼도 화면 소리만으로 송출할지는 호출하는 쪽에서 정한다. */
export async function screenAndMic(register: (t: MediaStreamTrack[]) => void, withMic: boolean) {
  if (!navigator.mediaDevices?.getDisplayMedia) {
    throw new PublishError('이 브라우저는 화면 공유를 지원하지 않습니다. PC의 Chrome이나 Edge를 쓰세요');
  }
  // 크롬·엣지: 공유 창에서 "탭 오디오 공유"·"시스템 오디오 공유"를 고를 수 있다. systemAudio는 그 칸을 보여 달라는 힌트다.
  const screen = await navigator.mediaDevices.getDisplayMedia({
    video: true,
    audio: true,
    systemAudio: 'include',
  } as DisplayMediaStreamOptions);
  register(screen.getTracks());
  const audio = [...screen.getAudioTracks()];
  if (withMic) {
    const mic = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
    register(mic.getTracks());
    audio.push(...mic.getAudioTracks());
  }
  return { video: screen.getVideoTracks()[0], audio, screenAudio: screen.getAudioTracks().length };
}
