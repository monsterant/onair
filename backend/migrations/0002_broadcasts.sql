-- 방송 (#3)

CREATE TABLE broadcasts (
    id          BIGSERIAL PRIMARY KEY,
    owner_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title       TEXT        NOT NULL,
    description TEXT        NOT NULL DEFAULT '',
    -- 공개용 영상 이름(시청 주소에 쓴다). 영문 소문자·숫자만 써서 OME 주소에 그대로 넣을 수 있다.
    stream_name TEXT        NOT NULL UNIQUE,
    -- 송출 키. 소유자에게만 보여 준다. OME 웹훅이 송출을 허락할 때 이 값과 비교한다.
    stream_key  TEXT        NOT NULL,
    -- ready: 대기(아직 송출 안 함, 또는 연결이 끊김) / live: 방송 중 / ended: 소유자가 종료(다시 송출 불가)
    status      TEXT        NOT NULL DEFAULT 'ready' CHECK (status IN ('ready', 'live', 'ended')),
    started_at  TIMESTAMPTZ,
    ended_at    TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX broadcasts_owner_idx ON broadcasts (owner_id);
CREATE INDEX broadcasts_status_created_idx ON broadcasts (status, created_at DESC);
