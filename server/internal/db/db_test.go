package db_test

import (
	"context"
	"encoding/json"
	"os"
	"strconv"
	"testing"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/db"
	"github.com/akinmoladun/edgesync/server/internal/models"
	"github.com/google/uuid"
)

func getTestDB(t *testing.T) *db.DB {
	t.Helper()

	host := os.Getenv("DB_HOST")
	if host == "" {
		host = "localhost"
	}
	portStr := os.Getenv("DB_PORT")
	port := 5432
	if portStr != "" {
		p, err := strconv.Atoi(portStr)
		if err == nil {
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

func TestDatabaseMigrationsAndRepository(t *testing.T) {
	database := getTestDB(t)
	defer database.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	// 1. Register Device
	deviceID := uuid.New()
	pubKey := make([]byte, 32)
	copy(pubKey, []byte("test-32-byte-public-key-abcdef!"))

	dev := models.Device{
		ID:        deviceID,
		PublicKey: pubKey,
		CreatedAt: time.Now().UTC(),
	}

	if err := database.RegisterDevice(ctx, dev); err != nil {
		t.Fatalf("failed to register device: %v", err)
	}

	gotDev, err := database.GetDevice(ctx, deviceID)
	if err != nil {
		t.Fatalf("failed to get registered device: %v", err)
	}
	if gotDev.ID != deviceID {
		t.Fatalf("expected device id %s, got %s", deviceID, gotDev.ID)
	}

	// 2. Insert Event
	eventID := uuid.New()
	event := models.Event{
		ID:          eventID,
		DeviceID:    deviceID,
		EntityType:  "revenue",
		EntityID:    "rev-001",
		EventType:   "revenue.created",
		Payload:     json.RawMessage(`{"amount": 1050, "currency": "USD"}`), // P1: integer cents
		VectorClock: models.VectorClock{deviceID.String(): 1},
		Signature:   []byte("test-signature-bytes"),
		CreatedAt:   time.Now().UTC(),
	}

	inserted, err := database.InsertEvent(ctx, event)
	if err != nil {
		t.Fatalf("failed to insert event: %v", err)
	}
	if !inserted {
		t.Fatalf("expected event to be inserted")
	}

	// Test idempotency: inserting same event ID again should return false, nil
	insertedAgain, err := database.InsertEvent(ctx, event)
	if err != nil {
		t.Fatalf("error on duplicate insert: %v", err)
	}
	if insertedAgain {
		t.Fatalf("expected duplicate insert to return false")
	}

	// 3. Upsert and Get Entity State
	state := models.EntityState{
		EntityType:  "revenue",
		EntityID:    "rev-001",
		State:       json.RawMessage(`{"total_amount": 1050}`),
		VectorClock: models.VectorClock{deviceID.String(): 1},
		UpdatedAt:   time.Now().UTC(),
	}

	if err := database.UpsertEntityState(ctx, state); err != nil {
		t.Fatalf("failed to upsert entity state: %v", err)
	}

	gotState, err := database.GetEntityState(ctx, "revenue", "rev-001")
	if err != nil {
		t.Fatalf("failed to get entity state: %v", err)
	}
	if gotState.EntityID != "rev-001" {
		t.Fatalf("expected rev-001, got %s", gotState.EntityID)
	}

	// 4. Record Conflict
	eventBID := uuid.New()
	eventB := models.Event{
		ID:          eventBID,
		DeviceID:    deviceID,
		EntityType:  "revenue",
		EntityID:    "rev-001",
		EventType:   "revenue.updated",
		Payload:     json.RawMessage(`{"amount": 2000}`),
		VectorClock: models.VectorClock{deviceID.String(): 2},
		Signature:   []byte("test-signature-bytes-b"),
		CreatedAt:   time.Now().UTC(),
	}
	_, err = database.InsertEvent(ctx, eventB)
	if err != nil {
		t.Fatalf("failed to insert eventB: %v", err)
	}

	conflict := models.Conflict{
		ID:         uuid.New(),
		EntityType: "revenue",
		EntityID:   "rev-001",
		EventAID:   eventID,
		EventBID:   eventBID,
		Status:     "unresolved",
		CreatedAt:  time.Now().UTC(),
	}

	if err := database.RecordConflict(ctx, conflict); err != nil {
		t.Fatalf("failed to record conflict: %v", err)
	}
}
