-- 방송 기록 요약(#110): 방송이 끝나면 주인에게 방송 시간·최고 시청자 수·채팅 수·후원 합계를 보여 준다.
-- 채팅은 메모리에만 있어 서버를 다시 켜면 사라지므로 수만 DB에 센다.
-- 최고 시청자 수는 화면이 방송을 불러올 때 받은 시청자 수(#85, WebRTC만)가 더 클 때 갱신한다.
-- first_started_at: 처음 방송을 시작한 시각. started_at은 송출을 멈췄다 다시 시작하면 새로 적혀서 따로 둔다.
ALTER TABLE broadcasts
    ADD COLUMN peak_viewers     INTEGER     NOT NULL DEFAULT 0,
    ADD COLUMN chat_count       INTEGER     NOT NULL DEFAULT 0,
    ADD COLUMN first_started_at TIMESTAMPTZ;
-- 이미 시작한 적이 있는 방송은 지금 아는 시작 시각으로 채운다
UPDATE broadcasts SET first_started_at = started_at WHERE started_at IS NOT NULL;
