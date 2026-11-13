-- 시청자 제한·천천히 모드 (#87)

-- 방송 주인이 막은 사용자. 막힌 사람은 그 방송에 채팅·댓글을 쓸 수 없다(읽기는 된다).
CREATE TABLE broadcast_bans (
    broadcast_id BIGINT      NOT NULL REFERENCES broadcasts (id) ON DELETE CASCADE,
    user_id      BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- 이 시각까지 막는다. NULL이면 해제할 때까지.
    until        TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (broadcast_id, user_id)
);

-- 천천히 모드: 사람마다 이 초에 한 번만 채팅(0이면 끔). 방송 주인은 예외.
ALTER TABLE broadcasts
    ADD COLUMN slow_mode_secs INTEGER NOT NULL DEFAULT 0
        CHECK (slow_mode_secs IN (0, 5, 10, 30));
