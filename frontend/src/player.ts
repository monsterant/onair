// 시청 플레이어. OvenPlayer(MIT)로 WebRTC를 먼저 시도하고, 안 되면 LL-HLS로 넘어간다.
// 주소는 화면과 같은 곳(https://도메인/app/...)이다. 배포에서는 Caddy가, 개발에서는 vite가 OME로 넘긴다.
import OvenPlayer from 'ovenplayer';
import Hls from 'hls.js';

// OvenPlayer는 LL-HLS를 재생할 때 window.Hls를 찾는다.
(window as unknown as { Hls: typeof Hls }).Hls = Hls;

/**
 * 플레이어를 만든다. 재생 오류가 나면 `onError`를 부른다(화면이 몇 초 뒤 다시 만든다, #27 V2).
 * 방송이 막 시작되면 영상이 아직 없어 오류가 날 수 있다. 특히 LL-HLS는 송출 뒤 몇 초 동안 준비가 안 된다(#10).
 */
export function startPlayer(containerId: string, streamName: string, onError?: () => void) {
  stopPlayer(containerId);
  const ws = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const p = OvenPlayer.create(containerId, {
    autoStart: true,
    // 브라우저는 소리 있는 영상의 자동 재생을 막는다. 음소거로 시작하고 사용자가 켠다.
    mute: true,
    autoFallback: true,
    sources: [
      { type: 'webrtc', label: '저지연(WebRTC)', file: `${ws}//${location.host}/app/${streamName}` },
      { type: 'll-hls', label: 'LL-HLS', file: `${location.origin}/app/${streamName}/llhls.m3u8` },
    ],
  });
  if (onError) p.on('error', () => onError());
  return p;
}

export function stopPlayer(containerId: string) {
  OvenPlayer.getPlayerByContainerId(containerId)?.remove();
}
