-- 방송 공지 고정 (#109)

-- 방송 주인이 채팅 칸 위에 고정하는 한 줄 공지. 빈 문자열이면 공지 없음.
ALTER TABLE broadcasts
    ADD COLUMN notice TEXT NOT NULL DEFAULT ''
        CHECK (char_length(notice) <= 100);
