// OvenPlayer(MIT)는 타입 파일이 없어서 쓰는 부분만 적는다.
declare module 'ovenplayer' {
  type Source = { type: 'webrtc' | 'll-hls' | 'hls'; file: string; label?: string };
  type Player = {
    remove(): void;
    on(event: string, cb: (data: unknown) => void): void;
    getMute(): boolean;
    setMute(muted: boolean): void;
    getVolume(): number;
    setVolume(volume: number): void;
  };
  const OvenPlayer: {
    create(containerId: string, options: Record<string, unknown> & { sources: Source[] }): Player;
    getPlayerByContainerId(id: string): Player | null;
  };
  export default OvenPlayer;
}
