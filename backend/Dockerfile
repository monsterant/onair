# 배포용 백엔드 이미지 (#5).
# 1단계에서 Rust로 빌드하고, 2단계에는 실행 파일만 담아 이미지를 작게 만든다.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
# root가 아닌 사용자로 실행한다
RUN useradd --system --no-create-home onair
COPY --from=build /src/target/release/onair-backend /usr/local/bin/onair-backend
USER onair
EXPOSE 8080
CMD ["onair-backend"]
