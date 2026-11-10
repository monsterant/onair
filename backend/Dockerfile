# 배포용 백엔드 이미지 (#5).
# 1단계에서 Rust로 빌드하고, 2단계에는 실행 파일만 담아 이미지를 작게 만든다.
FROM rust:1-bookworm AS build
WORKDIR /src

# 의존성만 먼저 빌드해서 층으로 남긴다(#21 C1).
# Cargo.toml·Cargo.lock이 그대로면 이 층을 다시 쓰므로, 소스만 바뀌었을 때 의존성을 다시 컴파일하지 않는다.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# 진짜 소스로 우리 코드만 다시 빌드한다. touch는 빈 파일보다 새 파일임을 cargo에 알려 준다.
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:bookworm-slim
# root가 아닌 사용자로 실행한다
RUN useradd --system --no-create-home onair
COPY --from=build /src/target/release/onair-backend /usr/local/bin/onair-backend
USER onair
EXPOSE 8080
CMD ["onair-backend"]
