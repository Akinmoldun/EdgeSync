package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/clock"
	"github.com/akinmoladun/edgesync/server/internal/crypto"
	"github.com/akinmoladun/edgesync/server/internal/db"
	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/google/uuid"
)

// PushRequest represents the payload sent by the edge client for synchronization.
type PushRequest struct {
	DeviceID uuid.UUID   `json:"device_id"`
	Events   []PushEvent `json:"events"`
}

// PushEvent represents a single event submitted for synchronization.
type PushEvent struct {
	ID          uuid.UUID          `json:"id"`
	EntityType  string             `json:"entity_type"`
	EntityID    string             `json:"entity_id"`
	EventType   string             `json:"event_type"`
	Payload     json.RawMessage    `json:"payload"`
	VectorClock models.VectorClock `json:"vector_clock"`
	Signature   json.RawMessage    `json:"signature"`
	CreatedAt   *time.Time         `json:"created_at,omitempty"`
}

// ConflictSummary summarizes a concurrent conflict detected during push.
type ConflictSummary struct {
	ID         uuid.UUID `json:"id"`
	EntityType string    `json:"entity_type"`
	EntityID   string    `json:"entity_id"`
	EventAID   uuid.UUID `json:"event_a_id"`
	EventBID   uuid.UUID `json:"event_b_id"`
}

// PushResponse conveys acceptance, idempotency, conflict, or rejection details.
type PushResponse struct {
	Accepted  []uuid.UUID       `json:"accepted"`
	Ignored   []uuid.UUID       `json:"ignored"`
	Conflicts []ConflictSummary `json:"conflicts"`
	Rejected  []uuid.UUID       `json:"rejected"`
}

func (s *Server) handlePush(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")

	var req PushRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "invalid request body: " + err.Error()})
		return
	}

	if req.DeviceID == uuid.Nil {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "device_id is required"})
		return
	}

	// 1. Authenticate device against database
	device, err := s.DB.GetDevice(r.Context(), req.DeviceID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound) {
			w.WriteHeader(http.StatusUnauthorized)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "device not registered"})
			return
		}
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "database error verifying device: " + err.Error()})
		return
	}

	resp := PushResponse{
		Accepted:  make([]uuid.UUID, 0),
		Ignored:   make([]uuid.UUID, 0),
		Conflicts: make([]ConflictSummary, 0),
		Rejected:  make([]uuid.UUID, 0),
	}

	// Process incoming events
	for _, pe := range req.Events {
		// 2. Verify signature
		sigBytes, err := crypto.ParseSignature(pe.Signature)
		if err != nil {
			resp.Rejected = append(resp.Rejected, pe.ID)
			continue
		}
		if !crypto.VerifySignature(device.PublicKey, pe.ID, pe.Payload, sigBytes) {
			resp.Rejected = append(resp.Rejected, pe.ID)
			continue
		}

		// 3. Check idempotency: if event already exists, ignore
		exists, err := s.DB.EventExists(r.Context(), pe.ID)
		if err != nil {
			w.WriteHeader(http.StatusInternalServerError)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "checking event existence: " + err.Error()})
			return
		}
		if exists {
			resp.Ignored = append(resp.Ignored, pe.ID)
			continue
		}

		createdAt := time.Now().UTC()
		if pe.CreatedAt != nil && !pe.CreatedAt.IsZero() {
			createdAt = *pe.CreatedAt
		}

		eventModel := models.Event{
			ID:          pe.ID,
			DeviceID:    req.DeviceID,
			EntityType:  pe.EntityType,
			EntityID:    pe.EntityID,
			EventType:   pe.EventType,
			Payload:     pe.Payload,
			VectorClock: pe.VectorClock,
			Signature:   sigBytes,
			CreatedAt:   createdAt,
		}

		// 4. Compare vector clock against entity_state
		currentState, err := s.DB.GetEntityState(r.Context(), pe.EntityType, pe.EntityID)
		if err != nil {
			if errors.Is(err, db.ErrNotFound) {
				// Initial state (dominates empty state)
				if _, err := s.DB.InsertEvent(r.Context(), eventModel); err != nil {
					w.WriteHeader(http.StatusInternalServerError)
					_ = json.NewEncoder(w).Encode(map[string]string{"error": "inserting initial event: " + err.Error()})
					return
				}

				newState := models.EntityState{
					EntityType:  pe.EntityType,
					EntityID:    pe.EntityID,
					State:       pe.Payload,
					VectorClock: pe.VectorClock,
					UpdatedAt:   createdAt,
				}
				if err := s.DB.UpsertEntityState(r.Context(), newState); err != nil {
					w.WriteHeader(http.StatusInternalServerError)
					_ = json.NewEncoder(w).Encode(map[string]string{"error": "upserting initial entity state: " + err.Error()})
					return
				}

				resp.Accepted = append(resp.Accepted, pe.ID)
				continue
			}

			w.WriteHeader(http.StatusInternalServerError)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "querying entity state: " + err.Error()})
			return
		}

		// Existing state exists, compare clocks
		rel := clock.Compare(pe.VectorClock, currentState.VectorClock)
		switch rel {
		case clock.Dominates:
			// Incoming event is causally newer
			if _, err := s.DB.InsertEvent(r.Context(), eventModel); err != nil {
				w.WriteHeader(http.StatusInternalServerError)
				_ = json.NewEncoder(w).Encode(map[string]string{"error": "inserting event: " + err.Error()})
				return
			}

			mergedClock := clock.Merge(currentState.VectorClock, pe.VectorClock)
			updatedState := models.EntityState{
				EntityType:  pe.EntityType,
				EntityID:    pe.EntityID,
				State:       pe.Payload,
				VectorClock: mergedClock,
				UpdatedAt:   createdAt,
			}
			if err := s.DB.UpsertEntityState(r.Context(), updatedState); err != nil {
				w.WriteHeader(http.StatusInternalServerError)
				_ = json.NewEncoder(w).Encode(map[string]string{"error": "updating entity state: " + err.Error()})
				return
			}

			resp.Accepted = append(resp.Accepted, pe.ID)

		case clock.Dominated, clock.Equal:
			// Incoming event is older or equal - record in ledger but do not update state
			if _, err := s.DB.InsertEvent(r.Context(), eventModel); err != nil {
				w.WriteHeader(http.StatusInternalServerError)
				_ = json.NewEncoder(w).Encode(map[string]string{"error": "inserting dominated event: " + err.Error()})
				return
			}
			resp.Ignored = append(resp.Ignored, pe.ID)

		case clock.Concurrent:
			// CONFLICT: DO NOT auto-merge financial data
			// Insert event into events table so foreign key constraint is satisfied
			if _, err := s.DB.InsertEvent(r.Context(), eventModel); err != nil {
				w.WriteHeader(http.StatusInternalServerError)
				_ = json.NewEncoder(w).Encode(map[string]string{"error": "inserting conflicting event: " + err.Error()})
				return
			}

			// Find latest event for entity to link as event_a_id
			latestEvent, err := s.DB.GetLatestEventForEntity(r.Context(), pe.EntityType, pe.EntityID)
			eventAID := pe.ID
			if err == nil && latestEvent != nil && latestEvent.ID != pe.ID {
				eventAID = latestEvent.ID
			}

			conflictID := uuid.New()
			conflict := models.Conflict{
				ID:         conflictID,
				EntityType: pe.EntityType,
				EntityID:   pe.EntityID,
				EventAID:   eventAID,
				EventBID:   pe.ID,
				Status:     "unresolved",
				CreatedAt:  createdAt,
			}

			if err := s.DB.RecordConflict(r.Context(), conflict); err != nil {
				w.WriteHeader(http.StatusInternalServerError)
				_ = json.NewEncoder(w).Encode(map[string]string{"error": "recording conflict: " + err.Error()})
				return
			}

			resp.Conflicts = append(resp.Conflicts, ConflictSummary{
				ID:         conflictID,
				EntityType: pe.EntityType,
				EntityID:   pe.EntityID,
				EventAID:   eventAID,
				EventBID:   pe.ID,
			})
		}
	}

	// Determine response HTTP status code
	if len(resp.Rejected) > 0 && len(resp.Accepted) == 0 && len(resp.Conflicts) == 0 && len(resp.Ignored) == 0 {
		w.WriteHeader(http.StatusBadRequest)
	} else if len(resp.Conflicts) > 0 {
		w.WriteHeader(http.StatusConflict)
	} else {
		w.WriteHeader(http.StatusOK)
	}

	if err := json.NewEncoder(w).Encode(resp); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}
