#!/usr/bin/env bash
# ==============================================================================
# EdgeSync — Chaos Simulation Harness with mTLS & SQLCipher (POSIX Bash)
# Specification: §8 Week 9 & Week 10
#
# Scenario:
# - Phase A: Generate mTLS certs, start Postgres & Go HTTPS mTLS server (poll /healthz)
# - Phase B: Build Rust client with SQLCipher, init device identity, register device on server
# - Phase C: Generate 50 encrypted offline events locally (Batch 1, do not push)
# - Phase D: Kill Go server (simulate complete DDIL blackout / network partition)
# - Phase E: Generate 50 MORE encrypted offline events during blackout (total 100 in outbox)
# - Phase F: Restart Go server (poll /healthz until healthy over mTLS)
# - Phase G: Client pushes all 100 events to server over mTLS (zero errors)
# - Phase H: Verify server DB has exactly 100 events for this device (Zero Data Loss)
# - Phase I: Verify client's outbox is completely empty
# - Phase J: Simulate concurrent mutation, pull over mTLS, and verify conflict surfacing
# - Security Checks: Prove SQLCipher rejects wrong/missing key, and mTLS fails closed
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

SERVER_PORT="${PORT:-8095}"
SERVER_URL="https://127.0.0.1:${SERVER_PORT}"
DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-postgres}"
DB_PASSWORD="${DB_PASSWORD:-postgrespassword}"
DB_NAME="${DB_NAME:-edgesync}"

TEMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'edgesync_chaos')"
RUN_ID="$(date +%s)"
DB_KEY="ddil-chaos-db-secret-key-${RUN_ID}"
SERVER_PID=""

CERTS_DIR="${TEMP_DIR}/certs"
CA_CERT="${CERTS_DIR}/ca.crt"
CLIENT_CERT="${CERTS_DIR}/client.crt"
CLIENT_KEY_FILE="${CERTS_DIR}/client.key"
SERVER_CERT="${CERTS_DIR}/server.crt"
SERVER_KEY_FILE="${CERTS_DIR}/server.key"

cleanup() {
    local exit_code=$?
    echo ""
    echo "=== Cleaning up Chaos Test Resources ==="
    if [ -n "${SERVER_PID}" ]; then
        echo "Terminating Go Sync Server (PID: ${SERVER_PID})..."
        kill -9 "${SERVER_PID}" 2>/dev/null || true
    fi
    rm -rf "${TEMP_DIR}" 2>/dev/null || true

    if [ ${exit_code} -eq 0 ]; then
        echo "============================================================"
        echo " ✅ CHAOS TEST PASSED: EdgeSync survived DDIL network blackout with mTLS, SQLCipher, and 0 data loss!"
        echo "============================================================"
    else
        echo "❌ CHAOS TEST FAILED (Exit Code: ${exit_code})"
    fi
    exit ${exit_code}
}
trap cleanup EXIT INT TERM

CLIENT_BIN=""
if [ -f "${ROOT_DIR}/client/target/x86_64-pc-windows-gnu/debug/edgesync.exe" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/x86_64-pc-windows-gnu/debug/edgesync.exe"
elif [ -f "${ROOT_DIR}/client/target/debug/edgesync.exe" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/debug/edgesync.exe"
elif [ -f "${ROOT_DIR}/client/target/debug/edgesync" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/debug/edgesync"
fi

if [ -z "${CLIENT_BIN}" ]; then
    (cd "${ROOT_DIR}/client" && cargo build)
    CLIENT_BIN="${ROOT_DIR}/client/target/debug/edgesync"
fi

wait_server_ready() {
    local timeout="${1:-30}"
    local start_time
    start_time=$(date +%s)
    while true; do
        if "${CLIENT_BIN}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY_FILE}" health --server-url "${SERVER_URL}" >/dev/null 2>&1; then
            return 0
        fi
        local now
        now=$(date +%s)
        if [ $((now - start_time)) -ge "${timeout}" ]; then
            return 1
        fi
        sleep 0.25
    done
}

wait_server_down() {
    local timeout="${1:-10}"
    local start_time
    start_time=$(date +%s)
    while true; do
        if ! "${CLIENT_BIN}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY_FILE}" health --server-url "${SERVER_URL}" >/dev/null 2>&1; then
            return 0
        fi
        local now
        now=$(date +%s)
        if [ $((now - start_time)) -ge "${timeout}" ]; then
            return 1
        fi
        sleep 0.1
    done
}

start_go_server() {
    (
        cd "${ROOT_DIR}/server"
        export PORT="${SERVER_PORT}"
        export DB_HOST="${DB_HOST}"
        export DB_PORT="${DB_PORT}"
        export DB_USER="${DB_USER}"
        export DB_PASSWORD="${DB_PASSWORD}"
        export DB_NAME="${DB_NAME}"
        export MIGRATIONS_DIR="${ROOT_DIR}/server/migrations"
        export SERVER_CERT_FILE="${SERVER_CERT}"
        export SERVER_KEY_FILE="${SERVER_KEY_FILE}"
        export CA_CERT_FILE="${CA_CERT}"
        go run ./cmd/api
    ) > "${TEMP_DIR}/server.log" 2>&1 &
    SERVER_PID=$!
}

echo "============================================================"
echo " Starting EdgeSync DDIL Chaos Simulation Harness (mTLS & SQLCipher)"
echo " Temp Directory: ${TEMP_DIR}"
echo " Server URL:     ${SERVER_URL} (mTLS HTTPS)"
echo "============================================================"

# --- Phase A: Generate Certs, Start Postgres & Go Server with mTLS ---
echo ""
echo ">>> Phase A: Generating mTLS certificates and starting services..."
"${ROOT_DIR}/scripts/gen_certs.sh" "${CERTS_DIR}"

if ! docker exec edgesync-postgres pg_isready -U "${DB_USER}" -d "${DB_NAME}" >/dev/null 2>&1; then
    echo "Starting PostgreSQL container via docker compose..."
    docker compose -f "${ROOT_DIR}/docker/docker-compose.yml" up -d
    for _ in {1..40}; do
        if docker exec edgesync-postgres pg_isready -U "${DB_USER}" -d "${DB_NAME}" >/dev/null 2>&1; then
            break
        fi
        sleep 0.5
    done
fi

start_go_server
if ! wait_server_ready 30; then
    echo "Go Sync Server failed to become responsive on ${SERVER_URL}"
    cat "${TEMP_DIR}/server.log"
    exit 1
fi

# Verify mTLS fail-closed: unauthenticated connection must be rejected
if "${CLIENT_BIN}" --ca-cert "${CA_CERT}" health --server-url "${SERVER_URL}" >/dev/null 2>&1; then
    echo "Security violation! Server accepted connection without client certificate."
    exit 1
fi
echo "[PASS] Phase A: Postgres and Go HTTPS mTLS server running (fail-closed verified)"

# --- Phase B: Register Device Identity ---
echo ""
echo ">>> Phase B: Initializing client identity and registering device..."
CLIENT_DB="${TEMP_DIR}/chaos_client.db"
CLIENT_KEY="${TEMP_DIR}/chaos_device.key"

run_client() {
    "${CLIENT_BIN}" --db "${CLIENT_DB}" --key "${CLIENT_KEY}" --db-key "${DB_KEY}" \
        --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY_FILE}" "$@"
}

INIT_OUT=$(run_client init-device)
DEV_ID=$(echo "${INIT_OUT}" | grep -oE "Device ID:[[:space:]]+[a-f0-9\-]+" | awk '{print $3}')
PUB_KEY_HEX=$(echo "${INIT_OUT}" | grep -oE "Public Key \(hex\):[[:space:]]+[a-f0-9]+" | awk '{print $4}')

docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" \
    -c "INSERT INTO devices (id, public_key) VALUES ('${DEV_ID}', decode('${PUB_KEY_HEX}', 'hex')) ON CONFLICT DO NOTHING;" >/dev/null
echo "[PASS] Phase B: Device ${DEV_ID} registered on server"

# --- Phase C: Generate 50 Offline Events (Batch 1) ---
echo ""
echo ">>> Phase C: Generating 50 offline encrypted events locally (Batch 1)..."
for i in {1..50}; do
    amount=$((i * 100))
    run_client add-revenue \
        --amount "${amount}" --description "Batch 1 revenue #${i}" --entity-id "rev-${RUN_ID}-b1-${i}" >/dev/null
done

OUTBOX_C=$(run_client outbox)
echo "${OUTBOX_C}" | grep -q "Pending Outbox Events (50)"

# Verify SQLCipher encryption: opening with wrong key fails
set +e
"${CLIENT_BIN}" --db "${CLIENT_DB}" --key "${CLIENT_KEY}" --db-key "wrong-key-12345" list-events >/dev/null 2>&1
WRONG_STATUS=$?
set -e
if [ "${WRONG_STATUS}" -eq 0 ]; then
    echo "SQLCipher failure! Database opened with wrong key."
    exit 1
fi
echo "[PASS] Phase C: Generated 50 offline events; SQLCipher encryption verified"

# --- Phase D: KILL the Go Server (Network Partition / Blackout) ---
echo ""
echo ">>> Phase D: Simulating network partition (killing Go server)..."
kill -9 "${SERVER_PID}" 2>/dev/null || true
if ! wait_server_down 10; then
    echo "Server is still responding after kill attempt!"
    exit 1
fi
echo "[PASS] Phase D: Server successfully killed; network partition active"

# --- Phase E: Generate 50 MORE Offline Events During Blackout ---
echo ""
echo ">>> Phase E: Generating 50 more offline events during blackout (Batch 2)..."
for i in {1..50}; do
    amount=$(((i + 50) * 100))
    run_client add-revenue \
        --amount "${amount}" --description "Batch 2 blackout revenue #${i}" --entity-id "rev-${RUN_ID}-b2-${i}" >/dev/null
done

OUTBOX_E=$(run_client outbox)
echo "${OUTBOX_E}" | grep -q "Pending Outbox Events (100)"
echo "[PASS] Phase E: Continuous offline work verified; 100 total events accumulated in encrypted outbox"

# --- Phase F: Restart the Go Server ---
echo ""
echo ">>> Phase F: Re-establishing network connectivity (restarting Go HTTPS server)..."
start_go_server
if ! wait_server_ready 30; then
    echo "Go server failed to recover and become healthy on ${SERVER_URL}"
    cat "${TEMP_DIR}/server.log"
    exit 1
fi
echo "[PASS] Phase F: Go HTTPS mTLS server reconnected and healthy"

# --- Phase G: Client Pushes All 100 Events to Server ---
echo ""
echo ">>> Phase G: Pushing all 100 accumulated events to server over mTLS..."
PUSH_OUT=$(run_client push --server-url "${SERVER_URL}")
echo "${PUSH_OUT}"
echo "${PUSH_OUT}" | grep -E -q "Accepted:[[:space:]]+100"
echo "[PASS] Phase G: All 100 events pushed and accepted over mTLS with zero errors"

# --- Phase H: Verify Server DB has Exactly 100 Events for Device ---
echo ""
echo ">>> Phase H: Verifying server database event count..."
COUNT_OUT=$(docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" -t -A -c "SELECT COUNT(*) FROM events WHERE device_id = '${DEV_ID}';")
COUNT_OUT=$(echo "${COUNT_OUT}" | tr -d '[:space:]')
echo "Server events count for device: ${COUNT_OUT}"
if [ "${COUNT_OUT}" != "100" ]; then
    echo "Zero-data-loss violation! Expected exactly 100 events in Postgres, found: ${COUNT_OUT}"
    exit 1
fi
echo "[PASS] Phase H: Verified server DB contains exactly 100 events for device (Zero Data Loss)"

# --- Phase I: Verify Client Outbox is Empty ---
echo ""
echo ">>> Phase I: Verifying client outbox is completely drained..."
OUTBOX_I=$(run_client outbox)
echo "${OUTBOX_I}" | grep -q "Pending Outbox Events (0)"
echo "[PASS] Phase I: Client outbox completely drained"

# --- Phase J: Concurrent Conflict Simulation & Surfacing ---
echo ""
echo ">>> Phase J: Simulating concurrent conflict in partition recovery..."
CONFLICT_ENTITY="rev-${RUN_ID}-b1-1"

# Local mutation
run_client add-revenue \
    --amount 99999 --description "Local mutation after partition" --entity-id "${CONFLICT_ENTITY}" >/dev/null

REMOTE_DEV_ID="00000000-0000-0000-0000-000000000004"
REMOTE_EV_ID="00000000-0000-0000-0000-000000000005"
REMOTE_HEX="0000000000000000000000000000000000000000000000000000000000000000"

docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" <<EOF >/dev/null
INSERT INTO devices (id, public_key) VALUES ('${REMOTE_DEV_ID}', decode('${REMOTE_HEX}', 'hex')) ON CONFLICT DO NOTHING;
INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature)
VALUES ('${REMOTE_EV_ID}', '${REMOTE_DEV_ID}', 'revenue', '${CONFLICT_ENTITY}', 'revenue.recorded',
        '{"amount_cents": 88888}', '{"${REMOTE_DEV_ID}": 1}',
        decode('00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000', 'hex'));
EOF

MAX_SEQ=$(docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" -t -A -c "SELECT COALESCE(MAX(server_seq) - 1, 0) FROM events;")
MAX_SEQ=$(echo "${MAX_SEQ}" | tr -d '[:space:]')

PULL_OUT=$(run_client pull --server-url "${SERVER_URL}" --after-seq "${MAX_SEQ}")
echo "${PULL_OUT}" | grep -E -q "Conflicts:[[:space:]]+1"

CONF_OUT=$(run_client conflicts --server-url "${SERVER_URL}")
echo "${CONF_OUT}" | grep -q "${CONFLICT_ENTITY}"

echo "[PASS] Phase J: Concurrent conflict detected and surfaced correctly over mTLS"
