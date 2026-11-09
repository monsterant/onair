# onair
실시간 방송 플랫폼

> 공모 제출용 README는 #6에서 완성한다. 지금은 로컬 실행 방법만 적는다.

## 로컬 실행

필요한 것: Docker, Rust(stable), Node.js

```bash
# 1. 환경변수 파일 만들기
cp .env.example .env

# 2. DB와 미디어 서버(OvenMediaEngine) 띄우기
docker compose up -d

# 3. 백엔드 (http://localhost:8080)
cd backend && cargo run

# 4. 프론트엔드 (http://localhost:5173)
cd frontend && npm install && npm run dev
```

프론트엔드 첫 화면에 `서버 ok, DB ok`가 보이면 연결된 것이다.

## 구성

| 폴더·파일 | 내용 |
|---|---|
| `backend/` | Rust(axum) 백엔드 |
| `frontend/` | Vite + TypeScript 화면 |
| `docker-compose.yml` | 로컬용 PostgreSQL, OvenMediaEngine |
| `.env.example` | 환경변수 예시 (진짜 비밀값 없음) |

## 사용한 오픈소스

| 이름 | 쓰는 곳 | 출처 |
|---|---|---|
| OvenMediaEngine | 방송 송출·시청 미디어 서버 | https://github.com/AirenSoft/OvenMediaEngine |
| PostgreSQL | DB | https://www.postgresql.org |
| axum, sqlx, tokio | 백엔드 | https://github.com/tokio-rs/axum, https://github.com/launchbadge/sqlx |
| Vite, TypeScript | 프론트엔드 | https://vite.dev, https://www.typescriptlang.org |
