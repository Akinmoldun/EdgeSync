package models

import (
	"encoding/json"
	"time"

	"github.com/google/uuid"
)

// Device represents an edge device authorized to push/pull events.
type Device struct {
	ID        uuid.UUID `json:"id"`
	PublicKey []byte    `json:"public_key"`
	CreatedAt time.Time `json:"created_at"`
}

// VectorClock represents causal event ordering across devices.
// Maps device_id string -> counter (i64).
type VectorClock map[string]int64

// Event represents an immutable client-generated state transition.
type Event struct {
	ID          uuid.UUID       `json:"id"`
	DeviceID    uuid.UUID       `json:"device_id"`
	EntityType  string          `json:"entity_type"`
	EntityID    string          `json:"entity_id"`
	EventType   string          `json:"event_type"`
	Payload     json.RawMessage `json:"payload"`
	VectorClock VectorClock     `json:"vector_clock"`
	Signature   []byte          `json:"signature"`
	ServerSeq   int64           `json:"server_seq,omitempty"`
	CreatedAt   time.Time       `json:"created_at"`
}

// EntityState represents the consolidated materialized state of an entity.
type EntityState struct {
	EntityType  string          `json:"entity_type"`
	EntityID    string          `json:"entity_id"`
	State       json.RawMessage `json:"state"`
	VectorClock VectorClock     `json:"vector_clock"`
	UpdatedAt   time.Time       `json:"updated_at"`
}

// Conflict represents concurrent incompatible mutations surfaced for review.
type Conflict struct {
	ID         uuid.UUID `json:"id"`
	EntityType string    `json:"entity_type"`
	EntityID   string    `json:"entity_id"`
	EventAID   uuid.UUID `json:"event_a_id"`
	EventBID   uuid.UUID `json:"event_b_id"`
	Status     string    `json:"status"`
	CreatedAt  time.Time `json:"created_at"`
}
