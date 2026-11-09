package api_test

import (
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

func TestSyncPull_InvalidAfterSeq(t *testing.T) {
	database := setupTestDB(t)
	server := api.NewServer(database)

	req := httptest.NewRequest(http.MethodGet, "/sync/pull?after_seq=invalid", nil)
	rr := httptest.NewRecorder()

	server.Router.ServeHTTP(rr, req)

	if rr.Code != http.StatusBadRequest {
		t.Fatalf("expected status 400 Bad Request, got %d. Body: %s", rr.Code, rr.Body.String())
	}
}

func TestSyncPull_NegativeAfterSeq(t *testing.T) {
	database := setupTestDB(t)
	server := api.NewServer(database)

	req := httptest.NewRequest(http.MethodGet, "/sync/pull?after_seq=-5", nil)
	rr := httptest.NewRecorder()

	server.Router.ServeHTTP(rr, req)

	if rr.Code != http.StatusBadRequest {
		t.Fatalf("expected status 400 Bad Request, got %d. Body: %s", rr.Code, rr.Body.String())
	}
}

func TestSyncPull_SuccessAndFiltering(t *testing.T) {
	database := setupTestDB(t)
	server := api.NewServer(database)

	ctx := context.Background()
	deviceID := uuid.New()
	dev := models.Device{
		ID:        deviceID,
		PublicKey: make([]byte, 32),
		CreatedAt: time.Now().UTC(),
	}
	if err := database.RegisterDevice(ctx, dev); err != nil {
		t.Fatalf("failed to register device: %v", err)
	}

	entityID := fmt.Sprintf("entity-%s", uuid.New().String())

	var startSeq int64
	_ = database.Pool.QueryRow(ctx, "SELECT COALESCE(MAX(server_seq), 0) FROM events").Scan(&startSeq)

	// Insert 2 sequential events
	ev1 := models.Event{
		ID:          uuid.New(),
		DeviceID:    deviceID,
		EntityType:  "revenue",
		EntityID:    entityID,
		EventType:   "revenue.recorded",
		Payload:     json.RawMessage(`{"amount_cents": 1000}`),
		VectorClock: models.VectorClock{deviceID.String(): 1},
		Signature:   make([]byte, 64),
		CreatedAt:   time.Now().UTC(),
	}
	if _, err := database.InsertEvent(ctx, ev1); err != nil {
		t.Fatalf("failed to insert ev1: %v", err)
	}

	ev2 := models.Event{
		ID:          uuid.New(),
		DeviceID:    deviceID,
		EntityType:  "revenue",
		EntityID:    entityID,
		EventType:   "revenue.recorded",
		Payload:     json.RawMessage(`{"amount_cents": 2000}`),
		VectorClock: models.VectorClock{deviceID.String(): 2},
		Signature:   make([]byte, 64),
		CreatedAt:   time.Now().UTC(),
	}
	if _, err := database.InsertEvent(ctx, ev2); err != nil {
		t.Fatalf("failed to insert ev2: %v", err)
	}

	// 1. Pull all events since startSeq
	reqAll := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/sync/pull?after_seq=%d", startSeq), nil)
	rrAll := httptest.NewRecorder()
	server.Router.ServeHTTP(rrAll, reqAll)

	if rrAll.Code != http.StatusOK {
		t.Fatalf("expected status 200 OK, got %d. Body: %s", rrAll.Code, rrAll.Body.String())
	}

	var allEvents []models.Event
	if err := json.Unmarshal(rrAll.Body.Bytes(), &allEvents); err != nil {
		t.Fatalf("failed to unmarshal pull response: %v", err)
	}

	if len(allEvents) < 2 {
		t.Fatalf("expected at least 2 events in pull, got %d", len(allEvents))
	}

	// Find ev1 and ev2 in response
	var foundEv1, foundEv2 *models.Event
	for i := range allEvents {
		if allEvents[i].ID == ev1.ID {
			foundEv1 = &allEvents[i]
		}
		if allEvents[i].ID == ev2.ID {
			foundEv2 = &allEvents[i]
		}
	}

	if foundEv1 == nil || foundEv2 == nil {
		t.Fatalf("ev1 or ev2 not found in pull response")
	}

	if foundEv1.ServerSeq >= foundEv2.ServerSeq {
		t.Fatalf("ev1 server_seq (%d) should be < ev2 server_seq (%d)", foundEv1.ServerSeq, foundEv2.ServerSeq)
	}

	// 2. Pull with after_seq = foundEv1.ServerSeq
	reqFiltered := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/sync/pull?after_seq=%d", foundEv1.ServerSeq), nil)
	rrFiltered := httptest.NewRecorder()
	server.Router.ServeHTTP(rrFiltered, reqFiltered)

	if rrFiltered.Code != http.StatusOK {
		t.Fatalf("expected status 200 OK, got %d. Body: %s", rrFiltered.Code, rrFiltered.Body.String())
	}

	var filteredEvents []models.Event
	if err := json.Unmarshal(rrFiltered.Body.Bytes(), &filteredEvents); err != nil {
		t.Fatalf("failed to unmarshal filtered pull response: %v", err)
	}

	for _, fe := range filteredEvents {
		if fe.ID == ev1.ID {
			t.Fatalf("ev1 should NOT be in filtered pull response where server_seq > %d", foundEv1.ServerSeq)
		}
	}

	// 3. Pull with after_seq = foundEv2.ServerSeq (should exclude ev2)
	reqAfterEv2 := httptest.NewRequest(http.MethodGet, fmt.Sprintf("/sync/pull?after_seq=%d", foundEv2.ServerSeq), nil)
	rrAfterEv2 := httptest.NewRecorder()
	server.Router.ServeHTTP(rrAfterEv2, reqAfterEv2)

	var afterEv2Events []models.Event
	if err := json.Unmarshal(rrAfterEv2.Body.Bytes(), &afterEv2Events); err != nil {
		t.Fatalf("failed to unmarshal afterEv2 response: %v", err)
	}

	for _, fe := range afterEv2Events {
		if fe.ID == ev1.ID || fe.ID == ev2.ID {
			t.Fatalf("neither ev1 nor ev2 should be in response after ev2")
		}
	}
}
