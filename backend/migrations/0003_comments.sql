-- 방송별 댓글 (#37)

CREATE TABLE comments (
    id           BIGSERIAL PRIMARY KEY,
    -- 방송이나 작성자가 지워지면 댓글도 함께 지운다
    broadcast_id BIGINT      NOT NULL REFERENCES broadcasts (id) ON DELETE CASCADE,
    author_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    body         TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 고친 적이 없으면 NULL. 화면에 "(수정됨)"을 보여 줄 때 쓴다.
    updated_at   TIMESTAMPTZ
);

CREATE INDEX comments_broadcast_idx ON comments (broadcast_id, id DESC);
CREATE INDEX comments_author_idx ON comments (author_id);
