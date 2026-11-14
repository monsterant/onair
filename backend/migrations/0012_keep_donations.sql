-- 방송을 지워도 후원 기록은 남긴다 (#128)

-- 지금까지는 방송을 지우면(주인이 지우거나 테스트 계정 24시간 정리) 적립된 후원까지 함께 지워졌다.
-- 이제는 방송 번호만 비우고(NULL) 금액·후원자·결제 키 기록은 남긴다. 후원자 탈퇴 때 donor_id를 비우는 것과 같은 방식.
ALTER TABLE donations ALTER COLUMN broadcast_id DROP NOT NULL;
ALTER TABLE donations DROP CONSTRAINT donations_broadcast_id_fkey;
ALTER TABLE donations
    ADD CONSTRAINT donations_broadcast_id_fkey
        FOREIGN KEY (broadcast_id) REFERENCES broadcasts (id) ON DELETE SET NULL;

-- 방송을 지울 때 그 방송의 후원(결제 전 주문 포함)을 빨리 찾도록. 지금 있는 색인은 적립된 것만 담는다.
CREATE INDEX donations_broadcast_idx ON donations (broadcast_id);
