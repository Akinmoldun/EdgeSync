package api_test

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"strconv"
	"testing"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/api"
	"github.com/akinmoladun/edgesync/server/internal/crypto"
	"github.com/akinmoladun/edgesync/server/internal/db"
	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/google/uuid"
)

func setupTestDB(t *testing.T) *db.DB {
	t.Helper()

	host := os.Getenv("DB_HOST")
	if host == "" {
		host = "localhost"
	}
	portStr := os.Getenv("DB_PORT")
	port := 5432
	if portStr != "" {
		if p, err := strconv.Atoi(portStr); err == nil {
			port = p
		}
	}
	user := os.Getenv("DB_USER")
	if user == "" {
		user = "postgres"
	}
	pass := os.Getenv("DB_PASSWORD")
	if pass == "" {
		pass = "postgrespassword"
	}
	dbname := os.Getenv("DB_NAME")
	if dbname == "" {
		dbname = "edgesync"
	}

	cfg := db.Config{
		Host:     host,
		Port:     port,
		User:     user,
		Password: pass,
		DBName:   dbname,
		SSLMode:  "disable",
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	database, err := db.New(ctx, cfg)
	if err != nil {
		t.Fatalf("failed to connect to test postgres database: %v", err)
	}

	// Apply migrations
	migrationsDir := "../../migrations"
	if err := database.RunMigrations(ctx, migrationsDir); err != nil {
		database.Close()
		t.Fatalf("failed to run migrations: %v", err)
	}

	return database
}

func TestSyncPush_UnauthorizedDevice(t *testing.T) {
	database := setupTestDB(t)
	defer database.Close()

	srv := api.NewServer(database)

	reqBody := api.PushRequest{
		DeviceID: uuid.New(), // Unregistered device
		Events:   []api.PushEvent{},
	}
	bodyBytes, err := json.Marshal(reqBody)
	if err != nil {
		t.Fatalf("marshaling request: %v", err)
	}

	req := httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(bodyBytes))
	rec := httptest.NewRecorder()

	srv.Router.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("expected status 401 Unauthorized, got %d", rec.Code)
	}
}

func TestSyncPush_InvalidSignature(t *testing.T) {
	database := setupTestDB(t)
	defer database.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	pub, _, err := crypto.GenerateKeypair()
	if err != nil {
		t.Fatalf("failed to generate keypair: %v", err)
	}

	deviceID := uuid.New()
	dev := models.Device{
		ID:        deviceID,
		PublicKey: pub,
		CreatedAt: time.Now().UTC(),
	}
	if err := database.RegisterDevice(ctx, dev); err != nil {
		t.Fatalf("registering device: %v", err)
	}

	srv := api.NewServer(database)

	eventID := uuid.New()
	fakeSig := make([]byte, 64)

	reqBody := api.PushRequest{
		DeviceID: deviceID,
		Events: []api.PushEvent{
			{
				ID:          eventID,
				EntityType:  "revenue",
				EntityID:    "rev-invalid-sig",
				EventType:   "revenue.created",
				Payload:     json.RawMessage(`{"amount_cents": 5000}`),
				VectorClock: models.VectorClock{deviceID.String(): 1},
				Signature:   json.RawMessage(`"` + hex.EncodeToString(fakeSig) + `"`),
			},
		},
	}
	bodyBytes, err := json.Marshal(reqBody)
	if err != nil {
		t.Fatalf("marshaling request: %v", err)
	}

	req := httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(bodyBytes))
	rec := httptest.NewRecorder()

	srv.Router.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("expected status 400 Bad Request, got %d", rec.Code)
	}

	var resp api.PushResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("unmarshaling response: %v", err)
	}

	if len(resp.Rejected) != 1 || resp.Rejected[0] != eventID {
		t.Fatalf("expected event %s to be rejected", eventID)
	}
}

func TestSyncPush_HappyPathAndIdempotency(t *testing.T) {
	database := setupTestDB(t)
	defer database.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	pub, priv, err := crypto.GenerateKeypair()
	if err != nil {
		t.Fatalf("generating keypair: %v", err)
	}

	deviceID := uuid.New()
	dev := models.Device{
		ID:        deviceID,
		PublicKey: pub,
		CreatedAt: time.Now().UTC(),
	}
	if err := database.RegisterDevice(ctx, dev); err != nil {
		t.Fatalf("registering device: %v", err)
	}

	srv := api.NewServer(database)

	// 1. Initial event (dominates empty state)
	event1ID := uuid.New()
	entityID := fmt.Sprintf("rev-e2e-%s", uuid.New().String())
	payload1 := []byte(`{"amount_cents": 10000, "currency": "USD"}`) // P1: integer cents
	sig1 := crypto.SignEvent(priv, event1ID, payload1)

	req1 := api.PushRequest{
		DeviceID: deviceID,
		Events: []api.PushEvent{
			{
				ID:          event1ID,
				EntityType:  "revenue",
				EntityID:    entityID,
				EventType:   "revenue.recorded",
				Payload:     payload1,
				VectorClock: models.VectorClock{deviceID.String(): 1},
				Signature:   json.RawMessage(`"` + hex.EncodeToString(sig1) + `"`),
			},
		},
	}
	body1, err := json.Marshal(req1)
	if err != nil {
		t.Fatalf("marshaling request 1: %v", err)
	}

	rec1 := httptest.NewRecorder()
	srv.Router.ServeHTTP(rec1, httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(body1)))

	if rec1.Code != http.StatusOK {
		t.Fatalf("expected 200 OK, got %d (body: %s)", rec1.Code, rec1.Body.String())
	}

	var resp1 api.PushResponse
	if err := json.Unmarshal(rec1.Body.Bytes(), &resp1); err != nil {
		t.Fatalf("unmarshaling resp 1: %v", err)
	}
	if len(resp1.Accepted) != 1 || resp1.Accepted[0] != event1ID {
		t.Fatalf("expected event1 to be accepted, got: %v", resp1)
	}

	// Verify state in DB
	state1, err := database.GetEntityState(ctx, "revenue", entityID)
	if err != nil {
		t.Fatalf("getting entity state: %v", err)
	}
	if state1.VectorClock[deviceID.String()] != 1 {
		t.Fatalf("expected clock 1, got %v", state1.VectorClock)
	}

	// 2. Idempotency test: submit event1 again
	rec1Again := httptest.NewRecorder()
	srv.Router.ServeHTTP(rec1Again, httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(body1)))

	if rec1Again.Code != http.StatusOK {
		t.Fatalf("expected 200 OK on idempotent re-submission, got %d", rec1Again.Code)
	}
	var resp1Again api.PushResponse
	if err := json.Unmarshal(rec1Again.Body.Bytes(), &resp1Again); err != nil {
		t.Fatalf("unmarshaling resp 1 again: %v", err)
	}
	if len(resp1Again.Ignored) != 1 || resp1Again.Ignored[0] != event1ID {
		t.Fatalf("expected event1 to be ignored as duplicate, got: %v", resp1Again)
	}

	// 3. Subsequent event that dominates: vector clock {deviceID: 2}
	event2ID := uuid.New()
	payload2 := []byte(`{"amount_cents": 15000, "currency": "USD"}`)
	sig2 := crypto.SignEvent(priv, event2ID, payload2)

	req2 := api.PushRequest{
		DeviceID: deviceID,
		Events: []api.PushEvent{
			{
				ID:          event2ID,
				EntityType:  "revenue",
				EntityID:    entityID,
				EventType:   "revenue.updated",
				Payload:     payload2,
				VectorClock: models.VectorClock{deviceID.String(): 2},
				Signature:   json.RawMessage(`"` + hex.EncodeToString(sig2) + `"`),
			},
		},
	}
	body2, err := json.Marshal(req2)
	if err != nil {
		t.Fatalf("marshaling request 2: %v", err)
	}

	rec2 := httptest.NewRecorder()
	srv.Router.ServeHTTP(rec2, httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(body2)))

	if rec2.Code != http.StatusOK {
		t.Fatalf("expected 200 OK, got %d (body: %s)", rec2.Code, rec2.Body.String())
	}

	state2, err := database.GetEntityState(ctx, "revenue", entityID)
	if err != nil {
		t.Fatalf("getting updated entity state: %v", err)
	}
	if state2.VectorClock[deviceID.String()] != 2 {
		t.Fatalf("expected clock 2, got %v", state2.VectorClock)
	}
}

func TestSyncPush_ConcurrentConflict(t *testing.T) {
	database := setupTestDB(t)
	defer database.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Register Device A
	pubA, privA, err := crypto.GenerateKeypair()
	if err != nil {
		t.Fatalf("generating keypair A: %v", err)
	}
	devA := models.Device{
		ID:        uuid.New(),
		PublicKey: pubA,
		CreatedAt: time.Now().UTC(),
	}
	if err := database.RegisterDevice(ctx, devA); err != nil {
		t.Fatalf("registering devA: %v", err)
	}

	// Register Device B
	pubB, privB, err := crypto.GenerateKeypair()
	if err != nil {
		t.Fatalf("generating keypair B: %v", err)
	}
	devB := models.Device{
		ID:        uuid.New(),
		PublicKey: pubB,
		CreatedAt: time.Now().UTC(),
	}
	if err := database.RegisterDevice(ctx, devB); err != nil {
		t.Fatalf("registering devB: %v", err)
	}

	srv := api.NewServer(database)

	entityID := fmt.Sprintf("rev-conflict-%s", uuid.New().String())

	// 1. Device A pushes initial event with clock {devA: 1}
	eventAID := uuid.New()
	payloadA := []byte(`{"amount_cents": 5000, "source": "devA"}`)
	sigA := crypto.SignEvent(privA, eventAID, payloadA)

	reqA := api.PushRequest{
		DeviceID: devA.ID,
		Events: []api.PushEvent{
			{
				ID:          eventAID,
				EntityType:  "revenue",
				EntityID:    entityID,
				EventType:   "revenue.recorded",
				Payload:     payloadA,
				VectorClock: models.VectorClock{devA.ID.String(): 1},
				Signature:   json.RawMessage(`"` + hex.EncodeToString(sigA) + `"`),
			},
		},
	}
	bodyA, _ := json.Marshal(reqA)
	recA := httptest.NewRecorder()
	srv.Router.ServeHTTP(recA, httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(bodyA)))
	if recA.Code != http.StatusOK {
		t.Fatalf("expected devA initial push to succeed, got %d", recA.Code)
	}

	// Current state in DB is {devA: 1}

	// 2. Device B pushes concurrent event with clock {devB: 1} without having seen devA's update!
	eventBID := uuid.New()
	payloadB := []byte(`{"amount_cents": 7500, "source": "devB"}`)
	sigB := crypto.SignEvent(privB, eventBID, payloadB)

	reqB := api.PushRequest{
		DeviceID: devB.ID,
		Events: []api.PushEvent{
			{
				ID:          eventBID,
				EntityType:  "revenue",
				EntityID:    entityID,
				EventType:   "revenue.recorded",
				Payload:     payloadB,
				VectorClock: models.VectorClock{devB.ID.String(): 1}, // Concurrent with {devA: 1}!
				Signature:   json.RawMessage(`"` + hex.EncodeToString(sigB) + `"`),
			},
		},
	}
	bodyB, _ := json.Marshal(reqB)
	recB := httptest.NewRecorder()
	srv.Router.ServeHTTP(recB, httptest.NewRequest(http.MethodPost, "/sync/push", bytes.NewReader(bodyB)))

	// Must return 409 Conflict
	if recB.Code != http.StatusConflict {
		t.Fatalf("expected status 409 Conflict, got %d (body: %s)", recB.Code, recB.Body.String())
	}

	var respB api.PushResponse
	if err := json.Unmarshal(recB.Body.Bytes(), &respB); err != nil {
		t.Fatalf("unmarshaling respB: %v", err)
	}

	if len(respB.Conflicts) != 1 {
		t.Fatalf("expected 1 conflict in response, got %d", len(respB.Conflicts))
	}
	conflict := respB.Conflicts[0]
	if conflict.EntityID != entityID {
		t.Fatalf("expected conflict entity %s, got %s", entityID, conflict.EntityID)
	}
	if conflict.EventBID != eventBID {
		t.Fatalf("expected conflict eventB %s, got %s", eventBID, conflict.EventBID)
	}

	// Hard Ban Check: Entity state MUST NOT be overwritten by concurrent update
	finalState, err := database.GetEntityState(ctx, "revenue", entityID)
	if err != nil {
		t.Fatalf("getting entity state: %v", err)
	}

	// State should still match devA's payload (not auto-merged!)
	var stateContent map[string]interface{}
	if err := json.Unmarshal(finalState.State, &stateContent); err != nil {
		t.Fatalf("unmarshaling state content: %v", err)
	}
	if stateContent["source"] != "devA" {
		t.Fatalf("state was improperly overwritten or merged: %v", stateContent)
	}
}
