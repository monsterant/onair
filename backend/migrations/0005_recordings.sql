-- 녹화와 다시보기 (#47)

CREATE TABLE recordings (
    id             BIGSERIAL PRIMARY KEY,
    -- 방송이 지워지면 녹화 기록도 지운다(파일은 백엔드 정리 작업이 지운다)
    broadcast_id   BIGINT      NOT NULL REFERENCES broadcasts (id) ON DELETE CASCADE,
    -- OME 녹화 id(startRecord/stopRecord에 쓴다)
    record_id      TEXT        NOT NULL UNIQUE,
    -- 녹화 폴더 안의 파일 이름. 영상 위치다.
    file_name      TEXT        NOT NULL UNIQUE,
    -- H.264는 mp4, VP8은 webm(mp4에는 VP8을 넣을 수 없다)
    format         TEXT        NOT NULL CHECK (format IN ('mp4', 'webm')),
    -- 녹화할 때의 방송 정보(방송 제목을 나중에 바꿔도 녹화 당시 제목이 남는다)
    title          TEXT        NOT NULL,
    owner_username TEXT        NOT NULL,
    -- recording: 녹화 중 / finishing: 멈추라고 했고 파일을 기다림 / ready: 다시보기 가능 / failed: 파일이 없음
    status         TEXT        NOT NULL DEFAULT 'recording' CHECK (status IN ('recording', 'finishing', 'ready', 'failed')),
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at       TIMESTAMPTZ,
    bytes          BIGINT      NOT NULL DEFAULT 0
);

CREATE INDEX recordings_broadcast_idx ON recordings (broadcast_id, started_at DESC);
CREATE INDEX recordings_ready_idx ON recordings (started_at DESC) WHERE status = 'ready';
-- 방송 하나에 녹화 중인 것은 하나만
CREATE UNIQUE INDEX recordings_one_active_per_broadcast ON recordings (broadcast_id) WHERE status = 'recording';
