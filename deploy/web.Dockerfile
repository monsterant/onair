# 배포용 웹 이미지 (#5): 프론트엔드를 빌드해서 Caddy에 넣는다.
# Caddy는 HTTPS 인증서를 자동으로 받고, 화면 파일을 내주고, /api와 영상 신호를 안쪽 서비스로 넘긴다.
FROM node:22-bookworm-slim AS build
WORKDIR /src
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend ./
RUN npm run build

FROM caddy:2.10.2
COPY deploy/Caddyfile /etc/caddy/Caddyfile
COPY --from=build /src/dist /srv
