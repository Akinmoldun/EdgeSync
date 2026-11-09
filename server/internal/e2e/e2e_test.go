package e2e_test

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/akinmoladun/edgesync/server/internal/api"
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

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	database, err := db.New(ctx, cfg)
	if err != nil {
		t.Fatalf("setupTestDB: failed to connect to Postgres: %v", err)
	}

	migDir := os.Getenv("MIGRATIONS_DIR")
	if migDir == "" {
		migDir = filepath.Join(findRepoRoot(t), "server", "migrations")
	}
	migCtx, migCancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer migCancel()
	if err := database.RunMigrations(migCtx, migDir); err != nil {
		t.Fatalf("setupTestDB: failed to run migrations: %v", err)
	}

	return database
}

func findRepoRoot(t *testing.T) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatalf("failed to get wd: %v", err)
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "PROGRESS.md")); err == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatalf("could not find repo root from %s", dir)
		}
		dir = parent
	}
}

// findClientBinary locates the precompiled edgesync binary or returns command to run via cargo.
func findClientBinary(t *testing.T) (string, []string) {
	t.Helper()
	root := findRepoRoot(t)

	// Try common build paths
	candidates := []string{
		filepath.Join(root, "client", "target", "x86_64-pc-windows-gnu", "debug", "edgesync.exe"),
		filepath.Join(root, "client", "target", "debug", "edgesync.exe"),
		filepath.Join(root, "client", "target", "debug", "edgesync"),
	}

	for _, c := range candidates {
		if info, err := os.Stat(c); err == nil && !info.IsDir() {
			return c, nil
		}
	}

	// Fallback to cargo run
	manifestPath := filepath.Join(root, "client", "Cargo.toml")
	return "cargo", []string{"run", "--manifest-path", manifestPath, "--"}
}

func runCLI(t *testing.T, bin string, baseArgs []string, extraArgs ...string) (string, error) {
	t.Helper()
	args := append([]string{}, baseArgs...)
	args = append(args, extraArgs...)

	cmd := exec.Command(bin, args...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	err := cmd.Run()
	output := stdout.String()
	if err != nil {
		return output, fmt.Errorf("CLI command failed (%v): stderr: %s, stdout: %s", err, stderr.String(), output)
	}
	return output, nil
}

func TestEndToEnd_FullSyncLifecycle(t *testing.T) {
	ctx := context.Background()
	database := setupTestDB(t)

	// 1. Start Go HTTP Server on dynamic port
	server := api.NewServer(database)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("failed to listen on random port: %v", err)
	}
	serverURL := fmt.Sprintf("http://127.0.0.1:%d", listener.Addr().(*net.TCPAddr).Port)

	httpServer := &http.Server{Handler: server.Router}
	go func() {
		_ = httpServer.Serve(listener)
	}()
	defer func() {
		_ = httpServer.Shutdown(context.Background())
	}()

	bin, baseArgs := findClientBinary(t)

	// 2. Prepare isolated client storage in temp dir
	tempDir := t.TempDir()
	dbPath := filepath.Join(tempDir, "client_e2e.db")
	keyPath := filepath.Join(tempDir, "client_device.key")

	clientArgs := func(cmdArgs ...string) []string {
		res := []string{"--db", dbPath, "--db-key", "test-secret-key-e2e", "--key", keyPath}
		res = append(res, cmdArgs...)
		return res
	}

	// 3. Step 1: Initialize device
	initOut, err := runCLI(t, bin, baseArgs, clientArgs("init-device")...)
	if err != nil {
		t.Fatalf("failed to init device: %v", err)
	}

	devIDRe := regexp.MustCompile(`Device ID:\s+([a-f0-9\-]+)`)
	pubKeyRe := regexp.MustCompile(`Public Key \(hex\):\s+([a-f0-9]+)`)

	devIDMatch := devIDRe.FindStringSubmatch(initOut)
	pubKeyMatch := pubKeyRe.FindStringSubmatch(initOut)
	if len(devIDMatch) < 2 || len(pubKeyMatch) < 2 {
		t.Fatalf("failed to parse device identity from output: %s", initOut)
	}

	deviceID, err := uuid.Parse(devIDMatch[1])
	if err != nil {
		t.Fatalf("invalid device UUID: %v", err)
	}
	pubKeyBytes, err := hex.DecodeString(pubKeyMatch[1])
	if err != nil {
		t.Fatalf("invalid public key hex: %v", err)
	}

	// Register device in central server database
	err = database.RegisterDevice(ctx, models.Device{
		ID:        deviceID,
		PublicKey: pubKeyBytes,
	})
	if err != nil {
		t.Fatalf("failed to register device in server DB: %v", err)
	}

	// 4. Step 2: Add Revenue transactions locally
	entityID1 := fmt.Sprintf("rev-e2e-%s", uuid.New().String()[:8])
	entityID2 := fmt.Sprintf("rev-e2e-%s", uuid.New().String()[:8])

	_, err = runCLI(t, bin, baseArgs, clientArgs(
		"add-revenue",
		"--amount", "5000",
		"--description", "E2E First Product Sale",
		"--entity-id", entityID1,
	)...)
	if err != nil {
		t.Fatalf("failed to add revenue 1: %v", err)
	}

	_, err = runCLI(t, bin, baseArgs, clientArgs(
		"add-revenue",
		"--amount", "2500",
		"--description", "E2E Second Product Sale",
		"--entity-id", entityID2,
	)...)
	if err != nil {
		t.Fatalf("failed to add revenue 2: %v", err)
	}

	// Verify outbox has 2 pending events
	outboxOut, err := runCLI(t, bin, baseArgs, clientArgs("outbox")...)
	if err != nil {
		t.Fatalf("failed to inspect outbox: %v", err)
	}
	if !strings.Contains(outboxOut, entityID1) || !strings.Contains(outboxOut, entityID2) {
		t.Fatalf("expected outbox to contain both entity events, got: %s", outboxOut)
	}

	// 5. Step 3: Push events to server
	pushOut, err := runCLI(t, bin, baseArgs, clientArgs(
		"push",
		"--server-url", serverURL,
	)...)
	if err != nil {
		t.Fatalf("failed to push events: %v", err)
	}
	if !regexp.MustCompile(`Accepted:\s+2`).MatchString(pushOut) {
		t.Fatalf("expected 2 accepted events, got: %s", pushOut)
	}

	// Verify outbox is now drained
	outboxDrained, err := runCLI(t, bin, baseArgs, clientArgs("outbox")...)
	if err != nil {
		t.Fatalf("failed to query outbox: %v", err)
	}
	if !strings.Contains(outboxDrained, "Pending Outbox Events (0)") {
		t.Fatalf("expected outbox to be drained after push, got: %s", outboxDrained)
	}

	// Verify server database state matches
	state1, err := database.GetEntityState(ctx, "revenue", entityID1)
	if err != nil {
		t.Fatalf("failed to get entity 1 from server DB: %v", err)
	}
	if !strings.Contains(string(state1.State), "5000") {
		t.Fatalf("expected server state to contain 5000 cents, got: %s", string(state1.State))
	}

	// Query latest server_seq after push
	var maxSeqAfterPush int64
	_ = database.Pool.QueryRow(ctx, "SELECT COALESCE(MAX(server_seq), 0) FROM events;").Scan(&maxSeqAfterPush)

	// 6. Step 4: Pull from server (idle pull, already synchronized)
	pullOut, err := runCLI(t, bin, baseArgs, clientArgs(
		"pull",
		"--server-url", serverURL,
		"--after-seq", strconv.FormatInt(maxSeqAfterPush, 10),
	)...)
	if err != nil {
		t.Fatalf("failed to pull events: %v", err)
	}
	if !strings.Contains(pullOut, "No new events to pull") {
		t.Fatalf("expected no new events to pull, got: %s", pullOut)
	}

	// 7. Step 5: Simulate concurrent conflict
	// Client records offline update for entity 1 locally
	_, err = runCLI(t, bin, baseArgs, clientArgs(
		"add-revenue",
		"--amount", "7500",
		"--description", "Client Local Offline Update",
		"--entity-id", entityID1,
	)...)
	if err != nil {
		t.Fatalf("failed to record client offline update: %v", err)
	}

	// Fake remote device creates concurrent event directly in server DB
	remoteDeviceID := uuid.New()
	_ = database.RegisterDevice(ctx, models.Device{
		ID:        remoteDeviceID,
		PublicKey: make([]byte, 32),
	})

	remoteEventID := uuid.New()
	remotePayload := json.RawMessage(`{"amount_cents": 8800, "source": "remote_edge_node"}`)
	// Remote clock {remoteDeviceID: 1} is concurrent with client's {deviceID: 2}
	remoteClock := models.VectorClock{
		remoteDeviceID.String(): 1,
	}

	remoteEvent := models.Event{
		ID:          remoteEventID,
		DeviceID:    remoteDeviceID,
		EntityType:  "revenue",
		EntityID:    entityID1,
		EventType:   "revenue.recorded",
		Payload:     remotePayload,
		VectorClock: remoteClock,
		Signature:   make([]byte, 64),
		CreatedAt:   time.Now().UTC(),
	}
	_, err = database.InsertEvent(ctx, remoteEvent)
	if err != nil {
		t.Fatalf("failed to insert remote concurrent event: %v", err)
	}

	// Client pulls from server: detects concurrent conflict!
	pullConflictOut, err := runCLI(t, bin, baseArgs, clientArgs(
		"pull",
		"--server-url", serverURL,
		"--after-seq", strconv.FormatInt(maxSeqAfterPush, 10),
	)...)
	if err != nil {
		t.Fatalf("failed to pull concurrent event: %v", err)
	}
	if !regexp.MustCompile(`Conflicts:\s+1`).MatchString(pullConflictOut) {
		t.Fatalf("expected pull to detect 1 conflict, got: %s", pullConflictOut)
	}

	// Hard ban verification: Client local state must NOT be silently overwritten!
	showStateOut, err := runCLI(t, bin, baseArgs, clientArgs(
		"show-state",
		"-t", "revenue",
		"-i", entityID1,
	)...)
	if err != nil {
		t.Fatalf("failed to show local entity state: %v", err)
	}
	if !strings.Contains(showStateOut, "7500") {
		t.Fatalf("local state must retain client offline update (7500), got: %s", showStateOut)
	}

	// 8. Step 6: Verify conflict is surfaced via CLI
	conflictsOut, err := runCLI(t, bin, baseArgs, clientArgs(
		"conflicts",
		"--server-url", serverURL,
	)...)
	if err != nil {
		t.Fatalf("failed to list conflicts: %v", err)
	}
	if !strings.Contains(conflictsOut, entityID1) {
		t.Fatalf("expected conflicts listing to surface %s, got: %s", entityID1, conflictsOut)
	}

	t.Logf("✅ End-to-End integration test successfully verified full lifecycle!")
}
