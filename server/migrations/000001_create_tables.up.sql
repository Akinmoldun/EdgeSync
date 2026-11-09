-- Migration: 000001_create_tables.up.sql

CREATE TABLE IF NOT EXISTS devices (
    id UUID PRIMARY KEY,
    public_key BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS events (
    id UUID PRIMARY KEY,
    device_id UUID NOT NULL REFERENCES devices(id),
    entity_type VARCHAR(64) NOT NULL,
    entity_id VARCHAR(128) NOT NULL,
    event_type VARCHAR(64) NOT NULL,
    payload JSONB NOT NULL,
    vector_clock JSONB NOT NULL,
    signature BYTEA NOT NULL,
    server_seq BIGSERIAL NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_events_server_seq UNIQUE (server_seq)
);

CREATE INDEX IF NOT EXISTS idx_events_server_seq ON events(server_seq);
CREATE INDEX IF NOT EXISTS idx_events_entity ON events(entity_type, entity_id);

CREATE TABLE IF NOT EXISTS entity_state (
    entity_type VARCHAR(64) NOT NULL,
    entity_id VARCHAR(128) NOT NULL,
    state JSONB NOT NULL,
    vector_clock JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (entity_type, entity_id)
);

CREATE TABLE IF NOT EXISTS conflicts (
    id UUID PRIMARY KEY,
    entity_type VARCHAR(64) NOT NULL,
    entity_id VARCHAR(128) NOT NULL,
    event_a_id UUID NOT NULL REFERENCES events(id),
    event_b_id UUID NOT NULL REFERENCES events(id),
    status VARCHAR(32) NOT NULL DEFAULT 'unresolved',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_conflicts_entity ON conflicts(entity_type, entity_id);
