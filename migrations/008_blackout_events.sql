CREATE TABLE blackout_events (
    id         SERIAL PRIMARY KEY,
    event      TEXT NOT NULL,
    start_time TIMESTAMPTZ NOT NULL,
    end_time   TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (end_time > start_time)
);

CREATE INDEX idx_blackout_active ON blackout_events(end_time);
