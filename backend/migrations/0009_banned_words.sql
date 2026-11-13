-- 채팅 금칙어 (#112)

-- 방송 주인이 정한 단어. 이 단어가 들어간 채팅은 서버가 거절한다. 소문자로 저장한다.
ALTER TABLE broadcasts
    ADD COLUMN banned_words TEXT[] NOT NULL DEFAULT '{}'
        CHECK (cardinality(banned_words) <= 30);
