CREATE TABLE transactions (
    hash VARCHAR(66) PRIMARY KEY,
    tx_type SMALLINT NOT NULL,
    sender VARCHAR(42),
    receiver VARCHAR(42),
    value_wei TEXT NOT NULL,
    gas_limit BIGINT NOT NULL,
    gas_price_or_max_fee_wei TEXT,
    max_priority_fee_wei TEXT,
    input_len INTEGER NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL
);