package db

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5"
)

var (
	ErrNotFound = errors.New("record not found")
)

// RegisterDevice inserts a new device public key into PostgreSQL.
func (d *DB) RegisterDevice(ctx context.Context, dev models.Device) error {
	query := `
		INSERT INTO devices (id, public_key, created_at)
		VALUES ($1, $2, $3)
		ON CONFLICT (id) DO NOTHING;
	`
	if dev.CreatedAt.IsZero() {
		dev.CreatedAt = time.Now().UTC()
	}
	_, err := d.Pool.Exec(ctx, query, dev.ID, dev.PublicKey, dev.CreatedAt)
	if err != nil {
		return fmt.Errorf("inserting device: %w", err)
	}
	return nil
}

// GetDevice retrieves a device by its ID.
func (d *DB) GetDevice(ctx context.Context, id uuid.UUID) (*models.Device, error) {
	query := `
		SELECT id, public_key, created_at
		FROM devices
		WHERE id = $1;
	`
	var dev models.Device
	err := d.Pool.QueryRow(ctx, query, id).Scan(&dev.ID, &dev.PublicKey, &dev.CreatedAt)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("querying device: %w", err)
	}
	return &dev, nil
}

// InsertEvent records an incoming edge event. Returns true if inserted, false if already existed (idempotent).
func (d *DB) InsertEvent(ctx context.Context, ev models.Event) (bool, error) {
	query := `
		INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature, created_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
		ON CONFLICT (id) DO NOTHING
		RETURNING server_seq;
	`
	clockJSON, err := json.Marshal(ev.VectorClock)
	if err != nil {
		return false, fmt.Errorf("marshaling vector clock: %w", err)
	}

	if ev.CreatedAt.IsZero() {
		ev.CreatedAt = time.Now().UTC()
	}

	var serverSeq int64
	err = d.Pool.QueryRow(ctx, query,
		ev.ID,
		ev.DeviceID,
		ev.EntityType,
		ev.EntityID,
		ev.EventType,
		ev.Payload,
		clockJSON,
		ev.Signature,
		ev.CreatedAt,
	).Scan(&serverSeq)

	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			// Idempotent: event ID already existed
			return false, nil
		}
		return false, fmt.Errorf("inserting event: %w", err)
	}

	return true, nil
}

// GetEntityState retrieves current consolidated state and clock for an entity.
func (d *DB) GetEntityState(ctx context.Context, entityType, entityID string) (*models.EntityState, error) {
	query := `
		SELECT entity_type, entity_id, state, vector_clock, updated_at
		FROM entity_state
		WHERE entity_type = $1 AND entity_id = $2;
	`
	var es models.EntityState
	var clockJSON []byte
	err := d.Pool.QueryRow(ctx, query, entityType, entityID).Scan(
		&es.EntityType,
		&es.EntityID,
		&es.State,
		&clockJSON,
		&es.UpdatedAt,
	)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("querying entity state: %w", err)
	}

	if err := json.Unmarshal(clockJSON, &es.VectorClock); err != nil {
		return nil, fmt.Errorf("unmarshaling vector clock: %w", err)
	}

	return &es, nil
}

// UpsertEntityState updates or inserts the entity's consolidated state.
func (d *DB) UpsertEntityState(ctx context.Context, es models.EntityState) error {
	query := `
		INSERT INTO entity_state (entity_type, entity_id, state, vector_clock, updated_at)
		VALUES ($1, $2, $3, $4, $5)
		ON CONFLICT (entity_type, entity_id)
		DO UPDATE SET
			state = EXCLUDED.state,
			vector_clock = EXCLUDED.vector_clock,
			updated_at = EXCLUDED.updated_at;
	`
	clockJSON, err := json.Marshal(es.VectorClock)
	if err != nil {
		return fmt.Errorf("marshaling vector clock: %w", err)
	}

	if es.UpdatedAt.IsZero() {
		es.UpdatedAt = time.Now().UTC()
	}

	_, err = d.Pool.Exec(ctx, query, es.EntityType, es.EntityID, es.State, clockJSON, es.UpdatedAt)
	if err != nil {
		return fmt.Errorf("upserting entity state: %w", err)
	}
	return nil
}

// RecordConflict logs a concurrent unmergeable conflict to the conflicts table.
func (d *DB) RecordConflict(ctx context.Context, c models.Conflict) error {
	query := `
		INSERT INTO conflicts (id, entity_type, entity_id, event_a_id, event_b_id, status, created_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7);
	`
	if c.ID == uuid.Nil {
		c.ID = uuid.New()
	}
	if c.Status == "" {
		c.Status = "unresolved"
	}
	if c.CreatedAt.IsZero() {
		c.CreatedAt = time.Now().UTC()
	}

	_, err := d.Pool.Exec(ctx, query, c.ID, c.EntityType, c.EntityID, c.EventAID, c.EventBID, c.Status, c.CreatedAt)
	if err != nil {
		return fmt.Errorf("recording conflict: %w", err)
	}
	return nil
}

// EventExists checks whether an event ID already exists in the database.
func (d *DB) EventExists(ctx context.Context, id uuid.UUID) (bool, error) {
	query := `SELECT 1 FROM events WHERE id = $1;`
	var dummy int
	err := d.Pool.QueryRow(ctx, query, id).Scan(&dummy)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return false, nil
		}
		return false, fmt.Errorf("checking event existence: %w", err)
	}
	return true, nil
}

// GetLatestEventForEntity retrieves the most recent event for an entity by server_seq.
func (d *DB) GetLatestEventForEntity(ctx context.Context, entityType, entityID string) (*models.Event, error) {
	query := `
		SELECT id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature, server_seq, created_at
		FROM events
		WHERE entity_type = $1 AND entity_id = $2
		ORDER BY server_seq DESC
		LIMIT 1;
	`
	var ev models.Event
	var clockJSON []byte
	err := d.Pool.QueryRow(ctx, query, entityType, entityID).Scan(
		&ev.ID,
		&ev.DeviceID,
		&ev.EntityType,
		&ev.EntityID,
		&ev.EventType,
		&ev.Payload,
		&clockJSON,
		&ev.Signature,
		&ev.ServerSeq,
		&ev.CreatedAt,
	)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("querying latest event: %w", err)
	}

	if err := json.Unmarshal(clockJSON, &ev.VectorClock); err != nil {
		return nil, fmt.Errorf("unmarshaling vector clock: %w", err)
	}

	return &ev, nil
}

// GetEventsAfterSeq retrieves all events where server_seq > afterSeq in ascending sequence order.
func (d *DB) GetEventsAfterSeq(ctx context.Context, afterSeq int64, limit int) ([]models.Event, error) {
	if limit <= 0 || limit > 1000 {
		limit = 500
	}

	query := `
		SELECT id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature, server_seq, created_at
		FROM events
		WHERE server_seq > $1
		ORDER BY server_seq ASC
		LIMIT $2;
	`
	rows, err := d.Pool.Query(ctx, query, afterSeq, limit)
	if err != nil {
		return nil, fmt.Errorf("querying events after seq %d: %w", afterSeq, err)
	}
	defer rows.Close()

	events := make([]models.Event, 0)
	for rows.Next() {
		var ev models.Event
		var clockJSON []byte
		err := rows.Scan(
			&ev.ID,
			&ev.DeviceID,
			&ev.EntityType,
			&ev.EntityID,
			&ev.EventType,
			&ev.Payload,
			&clockJSON,
			&ev.Signature,
			&ev.ServerSeq,
			&ev.CreatedAt,
		)
		if err != nil {
			return nil, fmt.Errorf("scanning event row: %w", err)
		}

		if err := json.Unmarshal(clockJSON, &ev.VectorClock); err != nil {
			return nil, fmt.Errorf("unmarshaling vector clock for event %s: %w", ev.ID, err)
		}

		events = append(events, ev)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating event rows: %w", err)
	}

	return events, nil
}

// GetUnresolvedConflicts returns all conflicts currently having status = 'unresolved'.
func (d *DB) GetUnresolvedConflicts(ctx context.Context) ([]models.Conflict, error) {
	query := `
		SELECT id, entity_type, entity_id, event_a_id, event_b_id, status, created_at
		FROM conflicts
		WHERE status = 'unresolved'
		ORDER BY created_at ASC;
	`
	rows, err := d.Pool.Query(ctx, query)
	if err != nil {
		return nil, fmt.Errorf("querying unresolved conflicts: %w", err)
	}
	defer rows.Close()

	conflicts := make([]models.Conflict, 0)
	for rows.Next() {
		var c models.Conflict
		err := rows.Scan(
			&c.ID,
			&c.EntityType,
			&c.EntityID,
			&c.EventAID,
			&c.EventBID,
			&c.Status,
			&c.CreatedAt,
		)
		if err != nil {
			return nil, fmt.Errorf("scanning conflict row: %w", err)
		}
		conflicts = append(conflicts, c)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating conflict rows: %w", err)
	}

	return conflicts, nil
}

// GetConflict retrieves a single conflict record by ID.
func (d *DB) GetConflict(ctx context.Context, id uuid.UUID) (*models.Conflict, error) {
	query := `
		SELECT id, entity_type, entity_id, event_a_id, event_b_id, status, created_at
		FROM conflicts
		WHERE id = $1;
	`
	var c models.Conflict
	err := d.Pool.QueryRow(ctx, query, id).Scan(
		&c.ID,
		&c.EntityType,
		&c.EntityID,
		&c.EventAID,
		&c.EventBID,
		&c.Status,
		&c.CreatedAt,
	)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("querying conflict %s: %w", id, err)
	}
	return &c, nil
}

// GetEvent retrieves an event by its ID.
func (d *DB) GetEvent(ctx context.Context, id uuid.UUID) (*models.Event, error) {
	query := `
		SELECT id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature, server_seq, created_at
		FROM events
		WHERE id = $1;
	`
	var ev models.Event
	var clockJSON []byte
	err := d.Pool.QueryRow(ctx, query, id).Scan(
		&ev.ID,
		&ev.DeviceID,
		&ev.EntityType,
		&ev.EntityID,
		&ev.EventType,
		&ev.Payload,
		&clockJSON,
		&ev.Signature,
		&ev.ServerSeq,
		&ev.CreatedAt,
	)
	if err != nil {
		if errors.Is(err, pgx.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, fmt.Errorf("querying event %s: %w", id, err)
	}

	if err := json.Unmarshal(clockJSON, &ev.VectorClock); err != nil {
		return nil, fmt.Errorf("unmarshaling vector clock for event %s: %w", id, err)
	}

	return &ev, nil
}

// MarkConflictResolved updates a conflict status to 'resolved'.
func (d *DB) MarkConflictResolved(ctx context.Context, id uuid.UUID) error {
	query := `
		UPDATE conflicts
		SET status = 'resolved'
		WHERE id = $1;
	`
	res, err := d.Pool.Exec(ctx, query, id)
	if err != nil {
		return fmt.Errorf("updating conflict status: %w", err)
	}
	if res.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}
