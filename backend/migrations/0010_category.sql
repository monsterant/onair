-- 방송 카테고리 (#113)

-- 정해진 분류 중 하나. 이미 있던 방송은 기타(etc).
ALTER TABLE broadcasts
    ADD COLUMN category TEXT NOT NULL DEFAULT 'etc'
        CHECK (category IN ('game', 'music', 'talk', 'study', 'etc'));
