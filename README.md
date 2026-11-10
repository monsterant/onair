# onair
실시간 방송 플랫폼

> 공모 제출용 README는 #6에서 완성한다. 지금은 로컬 실행 방법만 적는다.

## 로컬 실행

필요한 것: Docker, Rust(stable), Node.js

```bash
# 1. 환경변수 파일 만들기
cp .env.example .env
#    .env의 OME_ADMISSION_SECRET과 OME_API_TOKEN을 각각 16글자 이상 임의 값으로 바꾼다.
#    예시 값(change-me…) 그대로면 백엔드가 시작하지 않는다(docker compose도 이 값으로 OME를 띄운다).
#    OME_HOST_IP는 이 PC에서만 쓰면 127.0.0.1 그대로 둔다.

# 2. DB와 미디어 서버(OvenMediaEngine) 띄우기
docker compose up -d

# 3. 백엔드 (http://localhost:8080)
cd backend && cargo run

# 4. 프론트엔드 (http://localhost:5173)
cd frontend && npm install && npm run dev
```

프론트엔드 첫 화면에 `서버 ok, DB ok`가 보이면 연결된 것이다.

### 로그인과 테스트 계정
- 첫 화면에서 회원가입하거나 로그인한다(아이디: 영문·숫자·_ 3~20자, 비밀번호: 8자 이상).
- `.env`에 `DEMO_LOGIN=true`가 있으면 **demo1로 로그인**, **demo2로 로그인** 버튼이 동작한다. 비밀번호 없이 바로 로그인된다.
  - 두 계정은 "방송 소유자"와 "다른 사용자"를 비교할 때 쓴다.
  - 데모 계정은 누구나 같이 쓰는 공용 계정이다. 그래서 데모 계정으로 만든 방송은 다른 사람도 수정·종료·삭제할 수 있고, 송출 주소도 볼 수 있다. 시험용으로만 쓴다.
  - 한 사람이 동시에 가질 수 있는 끝나지 않은 방송은 5개까지다.

### 방송
- 로그인하면 **방송 만들기**로 방송을 만든다. 방송 상세 화면의 **송출 주소 보기**에서 WHIP 주소를 얻는다(OBS 30 이상의 WHIP 출력으로도 송출할 수 있다).
- 송출이 시작되면 상태가 "방송 중"이 되고, 상세 화면에서 누구나(로그인 없이) 시청할 수 있다.
- 소유자가 **방송 종료**를 누르면 송출이 끊기고 시청 화면도 "종료"로 바뀐다. 종료된 방송은 다시 송출할 수 없다.
- 로그인 쿠키는 Secure라서 `http://localhost`나 https에서만 저장된다. 같은 와이파이의 휴대폰에서 `http://<PC IP>`로 열면 로그인이 유지되지 않는다. 휴대폰은 배포 주소(https)로 시험한다.

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
| OvenPlayer (MIT) | 시청 플레이어(WebRTC, LL-HLS) | https://github.com/AirenSoft/OvenPlayer |
| hls.js (Apache-2.0) | LL-HLS 재생 | https://github.com/video-dev/hls.js |
| Caddy | 배포 서버 HTTPS·프록시 | https://caddyserver.com |
