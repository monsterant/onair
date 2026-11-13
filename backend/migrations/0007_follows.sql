-- 방송자 즐겨찾기 (#102)

-- user_id가 streamer_id를 즐겨찾기했다. 어느 쪽 계정이 지워져도 함께 지운다.
CREATE TABLE follows (
    user_id     BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    streamer_id BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, streamer_id),
    CHECK (user_id <> streamer_id)
);

-- 방송자 쪽 계정을 지울 때 CASCADE가 빨리 찾도록
CREATE INDEX follows_streamer_idx ON follows (streamer_id);
