package api_test

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/api"
	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/google/uuid"
)

func TestConflicts_ListAndResolve(t *testing.T) {
	database := setupTestDB(t)
	server := api.NewServer(database)

	ctx := context.Background()
	devA := uuid.New()
	devB := uuid.New()

	_ = database.RegisterDevice(ctx, models.Device{ID: devA, PublicKey: make([]byte, 32)})
	_ = database.RegisterDevice(ctx, models.Device{ID: devB, PublicKey: make([]byte, 32)})

	entityID := fmt.Sprintf("conflict-rev-%s", uuid.New().String())

	// Event A: {devA: 1}
	eventA := models.Event{
		ID:          uuid.New(),
		DeviceID:    devA,
		EntityType:  "revenue",
		EntityID:    entityID,
		EventType:   "revenue.recorded",
		Payload:     json.RawMessage(`{"amount_cents": 5000, "source": "A"}`),
		VectorClock: models.VectorClock{devA.String(): 1},
		Signature:   make([]byte, 64),
		CreatedAt:   time.Now().UTC(),
	}
	_, err := database.InsertEvent(ctx, eventA)
	if err != nil {
		t.Fatalf("failed to insert eventA: %v", err)
	}

	// Initial entity state set to Event A
	_ = database.UpsertEntityState(ctx, models.EntityState{
		EntityType:  "revenue",
		EntityID:    entityID,
		State:       eventA.Payload,
		VectorClock: eventA.VectorClock,
		UpdatedAt:   time.Now().UTC(),
	})

	// Event B: {devB: 1} (concurrent with Event A)
	eventB := models.Event{
		ID:          uuid.New(),
		DeviceID:    devB,
		EntityType:  "revenue",
		EntityID:    entityID,
		EventType:   "revenue.recorded",
		Payload:     json.RawMessage(`{"amount_cents": 7500, "source": "B"}`),
		VectorClock: models.VectorClock{devB.String(): 1},
		Signature:   make([]byte, 64),
		CreatedAt:   time.Now().UTC(),
	}
	_, err = database.InsertEvent(ctx, eventB)
	if err != nil {
		t.Fatalf("failed to insert eventB: %v", err)
	}

	conflictID := uuid.New()
	conflict := models.Conflict{
		ID:         conflictID,
		EntityType: "revenue",
		EntityID:   entityID,
		EventAID:   eventA.ID,
		EventBID:   eventB.ID,
		Status:     "unresolved",
		CreatedAt:  time.Now().UTC(),
	}
	if err := database.RecordConflict(ctx, conflict); err != nil {
		t.Fatalf("failed to record conflict: %v", err)
	}

	// 1. GET /conflicts - verify conflict is listed
	reqList := httptest.NewRequest(http.MethodGet, "/conflicts", nil)
	recList := httptest.NewRecorder()
	server.Router.ServeHTTP(recList, reqList)

	if recList.Code != http.StatusOK {
		t.Fatalf("expected 200 OK for GET /conflicts, got %d", recList.Code)
	}

	var conflictList []models.Conflict
	if err := json.Unmarshal(recList.Body.Bytes(), &conflictList); err != nil {
		t.Fatalf("failed to unmarshal conflicts: %v", err)
	}

	found := false
	for _, c := range conflictList {
		if c.ID == conflictID {
			found = true
			if c.Status != "unresolved" {
				t.Fatalf("expected status 'unresolved', got %s", c.Status)
			}
		}
	}
	if !found {
		t.Fatalf("conflict %s not found in GET /conflicts", conflictID)
	}

	// 2. POST /conflicts/{id}/resolve - select Event B as winner
	resolveReq := api.ResolveConflictRequest{
		WinnerEventID: eventB.ID,
	}
	resolveBody, _ := json.Marshal(resolveReq)
	reqResolve := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/conflicts/%s/resolve", conflictID), bytes.NewReader(resolveBody))
	recResolve := httptest.NewRecorder()
	server.Router.ServeHTTP(recResolve, reqResolve)

	if recResolve.Code != http.StatusOK {
		t.Fatalf("expected 200 OK for resolve, got %d (body: %s)", recResolve.Code, recResolve.Body.String())
	}

	var resolveResp api.ResolveConflictResponse
	if err := json.Unmarshal(recResolve.Body.Bytes(), &resolveResp); err != nil {
		t.Fatalf("unmarshaling resolve response: %v", err)
	}

	if resolveResp.Status != "resolved" || resolveResp.WinnerEventID != eventB.ID {
		t.Fatalf("unexpected resolve response: %+v", resolveResp)
	}

	// Verify state in DB matches Event B
	state, err := database.GetEntityState(ctx, "revenue", entityID)
	if err != nil {
		t.Fatalf("failed to get entity state: %v", err)
	}
	var expectedPayload, actualPayload map[string]interface{}
	_ = json.Unmarshal(eventB.Payload, &expectedPayload)
	_ = json.Unmarshal(state.State, &actualPayload)
	if fmt.Sprintf("%v", expectedPayload) != fmt.Sprintf("%v", actualPayload) {
		t.Fatalf("expected state payload to match event B %v, got %v", expectedPayload, actualPayload)
	}
	// Verify merged clock has both devA and devB
	if state.VectorClock[devA.String()] != 1 || state.VectorClock[devB.String()] != 1 {
		t.Fatalf("expected merged vector clock {devA: 1, devB: 1}, got: %+v", state.VectorClock)
	}

	// 3. GET /conflicts - conflict should no longer be listed
	recList2 := httptest.NewRecorder()
	server.Router.ServeHTTP(recList2, httptest.NewRequest(http.MethodGet, "/conflicts", nil))
	var conflictList2 []models.Conflict
	_ = json.Unmarshal(recList2.Body.Bytes(), &conflictList2)
	for _, c := range conflictList2 {
		if c.ID == conflictID {
			t.Fatalf("resolved conflict %s should no longer appear in unresolved /conflicts", conflictID)
		}
	}
}

func TestConflicts_ResolveNotFoundAndErrors(t *testing.T) {
	database := setupTestDB(t)
	server := api.NewServer(database)

	// Non-existent conflict ID
	fakeID := uuid.New()
	reqBody, _ := json.Marshal(api.ResolveConflictRequest{WinnerEventID: uuid.New()})
	req := httptest.NewRequest(http.MethodPost, fmt.Sprintf("/conflicts/%s/resolve", fakeID), bytes.NewReader(reqBody))
	rec := httptest.NewRecorder()
	server.Router.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("expected 404 Not Found for non-existent conflict, got %d", rec.Code)
	}
}
