-- 방송 예정 시각 (#114)

-- 방송 주인이 적어 둔 시작 예정 시각. 없으면 NULL.
ALTER TABLE broadcasts
    ADD COLUMN scheduled_at TIMESTAMPTZ;
