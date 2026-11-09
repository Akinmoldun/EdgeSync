package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/clock"
	"github.com/akinmoladun/edgesync/server/internal/db"
	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/go-chi/chi/v5"
	"github.com/google/uuid"
)

// ResolveConflictRequest specifies the human-selected winning event ID (§8 Week 7).
type ResolveConflictRequest struct {
	WinnerEventID uuid.UUID `json:"winner_event_id"`
}

// ResolveConflictResponse conveys the updated entity state and resolution details.
type ResolveConflictResponse struct {
	Status        string              `json:"status"`
	ConflictID    uuid.UUID           `json:"conflict_id"`
	WinnerEventID uuid.UUID           `json:"winner_event_id"`
	EntityState   *models.EntityState `json:"entity_state"`
}

func (s *Server) handleListConflicts(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")

	conflicts, err := s.DB.GetUnresolvedConflicts(r.Context())
	if err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to query conflicts: " + err.Error()})
		return
	}

	w.WriteHeader(http.StatusOK)
	if err := json.NewEncoder(w).Encode(conflicts); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}

func (s *Server) handleResolveConflict(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")

	conflictIDStr := chi.URLParam(r, "id")
	conflictID, err := uuid.Parse(conflictIDStr)
	if err != nil {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "invalid conflict ID format"})
		return
	}

	var req ResolveConflictRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "invalid request body: " + err.Error()})
		return
	}

	if req.WinnerEventID == uuid.Nil {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "winner_event_id is required"})
		return
	}

	// 1. Retrieve the conflict
	conflict, err := s.DB.GetConflict(r.Context(), conflictID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound) {
			w.WriteHeader(http.StatusNotFound)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "conflict not found"})
			return
		}
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to query conflict: " + err.Error()})
		return
	}

	if conflict.Status == "resolved" {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "conflict is already resolved"})
		return
	}

	// 2. Retrieve winning event
	winner, err := s.DB.GetEvent(r.Context(), req.WinnerEventID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound) {
			w.WriteHeader(http.StatusNotFound)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": "winning event not found"})
			return
		}
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to query winning event: " + err.Error()})
		return
	}

	// Ensure winning event belongs to the same entity
	if winner.EntityType != conflict.EntityType || winner.EntityID != conflict.EntityID {
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "winning event does not match conflict entity"})
		return
	}

	// 3. Compute merged vector clock and update entity state
	resolvedClock := winner.VectorClock
	currentState, err := s.DB.GetEntityState(r.Context(), conflict.EntityType, conflict.EntityID)
	if err == nil && currentState != nil {
		resolvedClock = clock.Merge(currentState.VectorClock, winner.VectorClock)
	}

	now := time.Now().UTC()
	newState := models.EntityState{
		EntityType:  conflict.EntityType,
		EntityID:    conflict.EntityID,
		State:       winner.Payload,
		VectorClock: resolvedClock,
		UpdatedAt:   now,
	}

	if err := s.DB.UpsertEntityState(r.Context(), newState); err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to apply resolved state: " + err.Error()})
		return
	}

	// 4. Mark conflict resolved
	if err := s.DB.MarkConflictResolved(r.Context(), conflictID); err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "failed to mark conflict resolved: " + err.Error()})
		return
	}

	resp := ResolveConflictResponse{
		Status:        "resolved",
		ConflictID:    conflictID,
		WinnerEventID: req.WinnerEventID,
		EntityState:   &newState,
	}

	w.WriteHeader(http.StatusOK)
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
}
