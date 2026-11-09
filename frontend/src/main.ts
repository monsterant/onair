// 뼈대 확인용 첫 화면: 백엔드 /health 결과를 보여 준다.
// 개발 중에는 vite 설정의 proxy가 /api 요청을 백엔드(8080)로 넘긴다.
const app = document.querySelector<HTMLElement>('#app')!;
app.textContent = 'onair: 백엔드 확인 중...';

fetch('/api/health')
  .then((res) => res.json())
  .then((h: { status: string; db: string }) => {
    app.textContent = `onair: 서버 ${h.status}, DB ${h.db}`;
  })
  .catch(() => {
    app.textContent = 'onair: 백엔드에 연결하지 못했습니다';
  });
