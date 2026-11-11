-- 도네이션 (#41)

CREATE TABLE donations (
    id           BIGSERIAL PRIMARY KEY,
    -- 토스에 보내는 주문번호. 결제창을 열기 전에 만든다.
    order_id     TEXT        NOT NULL UNIQUE,
    broadcast_id BIGINT      NOT NULL REFERENCES broadcasts (id) ON DELETE CASCADE,
    donor_id     BIGINT      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    amount       BIGINT      NOT NULL CHECK (amount BETWEEN 1000 AND 1000000),
    message      TEXT        NOT NULL DEFAULT '',
    -- pending: 결제창을 열었지만 아직 승인 전 / paid: 승인 확인 끝(적립) / failed: 거절·불일치
    status       TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'paid', 'failed')),
    -- 토스 결제 키. 승인 확인 뒤에 채운다. 같은 결제가 두 주문에 적립되지 않게 고유하게 둔다.
    payment_key  TEXT        UNIQUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    paid_at      TIMESTAMPTZ,
    -- 적립된 주문에는 반드시 결제 키와 시각이 있다
    CHECK (status <> 'paid' OR (payment_key IS NOT NULL AND paid_at IS NOT NULL))
);

CREATE INDEX donations_broadcast_paid_idx ON donations (broadcast_id, paid_at DESC) WHERE status = 'paid';
CREATE INDEX donations_donor_idx ON donations (donor_id);
