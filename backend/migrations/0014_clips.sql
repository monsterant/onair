-- 다시보기 장면 저장(#111). 영상 파일은 자르지 않고 재생 위치(밀리초)만 저장한다.
-- 다시보기나 만든 사람이 지워지면 장면도 지운다.
CREATE TABLE clips (
    id           BIGSERIAL PRIMARY KEY,
    recording_id BIGINT      NOT NULL REFERENCES recordings (id) ON DELETE CASCADE,
    user_id      BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title        TEXT        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 50),
    start_ms     INTEGER     NOT NULL CHECK (start_ms >= 0),
    end_ms       INTEGER     NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 끝은 시작보다 뒤, 길이는 2분까지
    CHECK (end_ms > start_ms AND end_ms - start_ms <= 120000)
);
CREATE INDEX clips_recording_idx ON clips (recording_id, created_at DESC);
