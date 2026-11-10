import { defineConfig } from 'vite';

export default defineConfig({
  server: {
    // 휴대폰 시험 때 같은 와이파이의 다른 기기에서도 열 수 있게 한다.
    host: true,
    proxy: {
      // /api/health -> http://localhost:8080/health
      '/api': {
        target: 'http://localhost:8080',
        rewrite: (path) => path.replace(/^\/api/, ''),
      },
      // 송출(WHIP)·WebRTC 신호(웹소켓)·LL-HLS를 이 PC의 OME로 넘긴다. 배포에서는 Caddy가 같은 일을 한다.
      '/app': {
        target: 'http://localhost:3333',
        ws: true,
      },
    },
  },
});
