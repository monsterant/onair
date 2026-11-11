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
- `.env`에 `DEMO_LOGIN=true`가 있으면 로그인 화면의 **테스트 계정으로 바로 시작** 버튼이 동작한다. 가입 없이 바로 로그인된다.
  - 누를 때마다 새 테스트 계정(`guest_` + 16진수 12자)이 만들어진다. 심사자마다 자기 계정을 쓰므로 방송 수 한도, 송출 주소, 수정·삭제 권한이 서로 섞이지 않는다.
  - "다른 사용자" 권한을 확인하려면 **시크릿 창이나 다른 브라우저**에서 한 번 더 누른다. 그 계정으로 첫 번째 계정의 방송을 수정·종료·삭제하면 거절된다(403).
  - 테스트 계정은 만든 지 24시간이 지나면 방송과 함께 지워진다(지금 송출 중이면 남긴다). 일반 가입에서는 `guest`, `demo`로 시작하는 아이디를 쓸 수 없다.
  - 한 사람이 동시에 가질 수 있는 끝나지 않은 방송은 5개까지다.
  - 자동 시험용 공용 계정 API(`POST /auth/demo/1`, `/auth/demo/2`)도 남아 있다. 화면 버튼은 없다.

### 방송
- 로그인하면 **방송 만들기**로 방송을 만든다.
- **PC 방송**: 방송 상세 화면의 **방송하기** 칸의 **PC 화면 공유**에서 **화면 공유로 방송 시작**을 누른다.
  - 공유 창에서 탭을 고르고 **탭 오디오 공유**를 켜면 탭 소리가 마이크 소리와 섞여 함께 간다. Windows에서 전체 화면을 고르면 **시스템 오디오 공유**도 고를 수 있다.
  - **송출 멈춤**을 누르거나 공유 창에서 **공유 중지**를 누르면 송출이 멈춘다. 방송은 "대기"로 돌아가고 다시 시작할 수 있다. 상세 화면을 떠나도 송출이 멈춘다.
- **휴대폰 방송**: 휴대폰 브라우저로 https 주소를 열고 방송 상세 화면의 **카메라 (휴대폰·PC)**에서 앞·뒤 카메라를 고른 뒤 **카메라로 방송 시작**을 누른다. 카메라 영상과 마이크 소리가 간다.
  - 송출하는 동안 화면이 꺼지지 않게 한다(지원하는 브라우저만).
  - **카메라 방송은 이 화면을 켜 둔 동안만 된다.** 다른 앱이나 다른 탭으로 가거나 화면을 끄면 송출을 멈추고 이유를 보여 준다(휴대폰 브라우저는 그때 카메라를 멈추거나 검은 화면을 보내기 때문). 돌아와서 다시 시작하면 된다.
  - 고른 카메라(앞·뒤)가 없으면 다른 카메라로 보내고 알려 준다.
  - 카메라는 PC에서도 쓸 수 있다.
- 다른 송출 프로그램: **송출 주소 보기**에서 WHIP 주소를 얻는다(OBS 30 이상의 WHIP 출력으로도 송출할 수 있다).
- 송출이 시작되면 상태가 "방송 중"이 되고, 상세 화면에서 누구나(로그인 없이) 시청할 수 있다.
- 소유자가 **방송 종료**를 누르면 송출이 끊기고 시청 화면도 "종료"로 바뀐다. 종료된 방송은 다시 송출할 수 없다.
- 로그인 쿠키는 Secure라서 `http://localhost`나 https에서만 저장된다. 같은 와이파이의 휴대폰에서 `http://<PC IP>`로 열면 로그인이 유지되지 않는다. 휴대폰은 배포 주소(https)로 시험한다.

### 지원 환경 (PC 방송)
| 브라우저 | 화면 공유 | 탭 소리 | 시스템 소리 |
|---|---|---|---|
| Chrome·Edge (Windows) | 됨 | 됨 (탭 공유) | 됨 (전체 화면 공유) |
| Chrome·Edge (macOS) | 됨 | 됨 (탭 공유) | 버전에 따라 다름 (안 될 수 있음) |
| Firefox, Safari | 됨 | 안 됨 | 안 됨 |
| 휴대폰 브라우저 | 화면 공유 없음 → 카메라 방송 | | |

### 지원 환경 (휴대폰 카메라 방송)
| 브라우저 | 카메라·마이크 송출 | 비고 |
|---|---|---|
| Android Chrome | 문서 기준 됨 (실기기 확인 전) | https 필요 |
| iPhone Safari (iOS 16.4 이상 권장) | 문서 기준 됨 (실기기 확인 전) | https 필요. 화면 꺼짐 막기는 16.4 이상 |
- 자동 시험은 휴대폰 화면을 흉내 낸 Chrome(375px, 가짜 카메라)으로 했다. 진짜 휴대폰 시험은 사용자가 따로 한다.

- 표는 브라우저 문서를 바탕으로 적었다. 자동 시험은 Windows Chrome(가짜 화면·마이크)으로 했다.
- 소리는 **탭**이나 **전체 화면**을 공유할 때만 고를 수 있다. **창**을 공유하면 소리 칸이 없다.
- macOS는 처음 쓸 때 "시스템 설정 → 개인정보 보호 및 보안 → 화면 기록"에서 브라우저를 허용하고 브라우저를 다시 열어야 한다.
- 시청은 WebRTC를 먼저 쓰고, 안 되면 LL-HLS로 넘어간다(최신 Chrome·Edge·Firefox·Safari, 휴대폰 브라우저).

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
