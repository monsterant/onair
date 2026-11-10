-- 회원과 로그인 세션 (#4)

CREATE TABLE users (
    id            BIGSERIAL PRIMARY KEY,
    -- 영문 소문자·숫자·_ 3~20자. 같은 아이디는 DB가 막는다(동시에 두 번 가입해도 하나만 성공).
    username      TEXT        NOT NULL UNIQUE,
    -- argon2id 해시(PHC 문자열). 비밀번호 원문은 저장하지 않는다.
    password_hash TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    -- 쿠키에 넣은 토큰의 SHA-256. 원문을 저장하지 않아서, DB가 새도 그 값으로 바로 로그인할 수 없다.
    token_hash BYTEA       PRIMARY KEY,
    user_id    BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
