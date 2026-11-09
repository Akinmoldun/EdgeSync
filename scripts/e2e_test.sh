#!/usr/bin/env bash
# ==============================================================================
# EdgeSync — End-to-End Integration Test Suite (§8 Week 10)
#
# Tests the complete synchronization lifecycle with SQLCipher DB Encryption & mTLS:
# 1. Spins up PostgreSQL database via Docker (if not already running)
# 2. Generates local mTLS PKI certificates
# 3. Builds and starts the Go Sync Server with mTLS enabled
# 4. Builds the Rust Client CLI binary with SQLCipher
# 5. Executes full client lifecycle over mTLS:
#    - Initialize device identity and register with server
#    - Add revenue transactions locally into encrypted SQLite DB
#    - Verify outbox and local materialized state
#    - Push to server over mTLS and verify server database state & outbox clearing
#    - Pull from server (idle / up-to-date verification)
#    - Simulate concurrent mutation from another device
#    - Pull from server and verify concurrent conflict detection
#    - Verify local un-synced state is NOT overwritten (Hard ban)
#    - Surface conflicts via CLI
# 6. Clean up background server, temporary files, and Docker containers
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

SERVER_PORT="${PORT:-8088}"
SERVER_URL="https://127.0.0.1:${SERVER_PORT}"
DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-postgres}"
DB_PASSWORD="${DB_PASSWORD:-postgrespassword}"
DB_NAME="${DB_NAME:-edgesync}"
DB_KEY="e2e-sqlcipher-passphrase-secret-42"

TEMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t 'edgesync_e2e')"
CERTS_DIR="${TEMP_DIR}/certs"
mkdir -p "${CERTS_DIR}"

SERVER_PID=""
DOCKER_STARTED="false"

echo "============================================================"
echo " Starting EdgeSync End-to-End Integration Test (mTLS + SQLCipher)"
echo " Working directory: ${TEMP_DIR}"
echo " Server URL:        ${SERVER_URL}"
echo "============================================================"

cleanup() {
    local exit_code=$?
    echo ""
    echo "=== Cleaning up resources ==="
    if [ -n "${SERVER_PID}" ]; then
        echo "Stopping Go Sync Server (PID: ${SERVER_PID})..."
        kill "${SERVER_PID}" 2>/dev/null || true
        wait "${SERVER_PID}" 2>/dev/null || true
    fi

    if [ "${DOCKER_STARTED}" = "true" ]; then
        echo "Stopping Docker PostgreSQL container..."
        docker compose -f "${ROOT_DIR}/docker/docker-compose.yml" down || true
    fi

    echo "Removing temporary test files..."
    rm -rf "${TEMP_DIR}" 2>/dev/null || true

    if [ ${exit_code} -eq 0 ]; then
        echo "✅ End-to-End Integration Test PASSED (mTLS + SQLCipher)!"
    else
        echo "❌ End-to-End Integration Test FAILED (Exit Code: ${exit_code})"
    fi
    exit ${exit_code}
}
trap cleanup EXIT INT TERM

# Step 1: Ensure PostgreSQL is running
echo "Checking PostgreSQL connection on ${DB_HOST}:${DB_PORT}..."
if ! nc -z "${DB_HOST}" "${DB_PORT}" 2>/dev/null && ! (echo > /dev/tcp/"${DB_HOST}"/"${DB_PORT}") 2>/dev/null; then
    echo "PostgreSQL not reachable. Starting docker compose..."
    docker compose -f "${ROOT_DIR}/docker/docker-compose.yml" up -d
    DOCKER_STARTED="true"
    echo "Waiting for PostgreSQL to become ready..."
    sleep 3
fi

# Step 2: Generate local mTLS certificates
echo "Generating local mTLS certificates..."
"${ROOT_DIR}/scripts/gen_certs.sh" "${CERTS_DIR}"

CA_CERT="${CERTS_DIR}/ca.crt"
SERVER_CERT="${CERTS_DIR}/server.crt"
SERVER_KEY="${CERTS_DIR}/server.key"
CLIENT_CERT="${CERTS_DIR}/client.crt"
CLIENT_KEY="${CERTS_DIR}/client.key"

# Step 3: Build Rust client binary
echo "Building Rust client binary..."
(cd "${ROOT_DIR}/client" && cargo build)

# Locate the compiled edgesync binary
CLIENT_BIN=""
if [ -f "${ROOT_DIR}/client/target/x86_64-pc-windows-gnu/debug/edgesync.exe" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/x86_64-pc-windows-gnu/debug/edgesync.exe"
elif [ -f "${ROOT_DIR}/client/target/debug/edgesync.exe" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/debug/edgesync.exe"
elif [ -f "${ROOT_DIR}/client/target/debug/edgesync" ]; then
    CLIENT_BIN="${ROOT_DIR}/client/target/debug/edgesync"
fi

if [ -z "${CLIENT_BIN}" ]; then
    echo "Could not find prebuilt binary. Running client via cargo run..."
    run_client() {
        cargo run --manifest-path "${ROOT_DIR}/client/Cargo.toml" --quiet -- "$@"
    }
else
    echo "Using client binary: ${CLIENT_BIN}"
    run_client() {
        "${CLIENT_BIN}" "$@"
    }
fi

# Step 4: Start Go Sync Server on designated port with mTLS
echo "Starting Go Sync Server on port ${SERVER_PORT} with mTLS..."
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
    export SERVER_KEY_FILE="${SERVER_KEY}"
    export CA_CERT_FILE="${CA_CERT}"
    go run ./cmd/api
) > "${TEMP_DIR}/server.log" 2>&1 &
SERVER_PID=$!

echo "Waiting for server to become responsive on ${SERVER_URL}/healthz..."
for i in {1..30}; do
    if run_client health --server-url "${SERVER_URL}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY}" >/dev/null 2>&1; then
        echo "Server is ready with mTLS!"
        break
    fi
    sleep 1
    if [ $i -eq 30 ]; then
        echo "Server failed to start within 30 seconds. Log output:"
        cat "${TEMP_DIR}/server.log"
        exit 1
    fi
done

# Step 5: Initialize client device identity with SQLCipher
CLIENT_DB="${TEMP_DIR}/client.db"
CLIENT_DEVICE_KEY="${TEMP_DIR}/device.key"

echo "Initializing edge client identity with SQLCipher..."
INIT_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" init-device)
echo "${INIT_OUT}"

DEVICE_ID=$(echo "${INIT_OUT}" | grep -oE "Device ID:[[:space:]]+[a-f0-9\-]+" | awk '{print $3}')
PUB_KEY_HEX=$(echo "${INIT_OUT}" | grep -oE "Public Key \(hex\):[[:space:]]+[a-f0-9]+" | awk '{print $4}')

echo "Extracted Device ID: ${DEVICE_ID}"
echo "Extracted Public Key: ${PUB_KEY_HEX}"

# Register device in central server Postgres database
echo "Registering device in PostgreSQL devices table..."
docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" \
    -c "INSERT INTO devices (id, public_key) VALUES ('${DEVICE_ID}', decode('${PUB_KEY_HEX}', 'hex')) ON CONFLICT DO NOTHING;" 2>/dev/null || \
PGPASSWORD="${DB_PASSWORD}" psql -h "${DB_HOST}" -p "${DB_PORT}" -U "${DB_USER}" -d "${DB_NAME}" \
    -c "INSERT INTO devices (id, public_key) VALUES ('${DEVICE_ID}', decode('${PUB_KEY_HEX}', 'hex')) ON CONFLICT DO NOTHING;"

# Step 6: Add revenue transactions locally
E2E_ENTITY_1="rev-e2e-$(date +%s)-1"
E2E_ENTITY_2="rev-e2e-$(date +%s)-2"

echo "Adding revenue transaction 1 (${E2E_ENTITY_1}): 5000 cents ($50.00)..."
run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" add-revenue \
    --amount 5000 --description "E2E First Sale" --entity-id "${E2E_ENTITY_1}"

echo "Adding revenue transaction 2 (${E2E_ENTITY_2}): 2500 cents ($25.00)..."
run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" add-revenue \
    --amount 2500 --description "E2E Second Sale" --entity-id "${E2E_ENTITY_2}"

# Verify outbox has 2 events
echo "Checking outbox..."
OUTBOX_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" outbox)
echo "${OUTBOX_OUT}"
echo "${OUTBOX_OUT}" | grep -q "${E2E_ENTITY_1}"
echo "${OUTBOX_OUT}" | grep -q "${E2E_ENTITY_2}"

# Step 7: Push events to server over mTLS
echo "Pushing outbox events to server over mTLS..."
PUSH_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" push \
    --server-url "${SERVER_URL}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY}")
echo "${PUSH_OUT}"
echo "${PUSH_OUT}" | grep -q "Accepted:[[:space:]]+2"

# Verify outbox is drained
OUTBOX_AFTER=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" outbox)
echo "${OUTBOX_AFTER}" | grep -q "Pending Outbox Events (0)"

# Query latest server_seq in Postgres for isolated pull
MAX_SEQ=$(docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" -t -A -c "SELECT COALESCE(MAX(server_seq), 0) FROM events;" 2>/dev/null || echo "0")
MAX_SEQ=$(echo "${MAX_SEQ}" | tr -d '[:space:]')

# Step 8: Pull from server (idle pull) over mTLS
echo "Pulling events from server over mTLS..."
PULL_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" pull \
    --server-url "${SERVER_URL}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY}" \
    --after-seq "${MAX_SEQ}")
echo "${PULL_OUT}"
echo "${PULL_OUT}" | grep -q "No new events to pull"

# Step 9: Simulate concurrent conflict
echo "Simulating concurrent conflict on entity ${E2E_ENTITY_1}..."
# Client creates an offline mutation
run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" add-revenue \
    --amount 7500 --description "Client Local Offline Update" --entity-id "${E2E_ENTITY_1}"

# Remote device creates concurrent event directly in server DB
REMOTE_DEV_ID="00000000-0000-0000-0000-000000000002"
REMOTE_EVENT_ID="00000000-0000-0000-0000-000000000003"
REMOTE_PUB_KEY="0000000000000000000000000000000000000000000000000000000000000000"

docker exec -i edgesync-postgres psql -U "${DB_USER}" -d "${DB_NAME}" <<EOF 2>/dev/null || \
PGPASSWORD="${DB_PASSWORD}" psql -h "${DB_HOST}" -p "${DB_PORT}" -U "${DB_USER}" -d "${DB_NAME}" <<EOF
INSERT INTO devices (id, public_key) VALUES ('${REMOTE_DEV_ID}', decode('${REMOTE_PUB_KEY}', 'hex')) ON CONFLICT DO NOTHING;
INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature)
VALUES ('${REMOTE_EVENT_ID}', '${REMOTE_DEV_ID}', 'revenue', '${E2E_ENTITY_1}', 'revenue.recorded',
        '{"amount_cents": 8800, "source": "remote_edge_node"}',
        '{"${REMOTE_DEV_ID}": 1}',
        decode('00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000', 'hex'));
EOF

# Client pulls from server: detects conflict!
echo "Client pulling from server to trigger conflict detection over mTLS..."
PULL_CONF_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" pull \
    --server-url "${SERVER_URL}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY}" \
    --after-seq "${MAX_SEQ}")
echo "${PULL_CONF_OUT}"
echo "${PULL_CONF_OUT}" | grep -E -q "Conflicts:[[:space:]]+1"

# Verify local un-synced state was NOT overwritten (Hard ban)
STATE_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" show-state -t revenue -i "${E2E_ENTITY_1}")
echo "${STATE_OUT}"
echo "${STATE_OUT}" | grep -q "7500"

# Surface conflicts via CLI over mTLS
echo "Listing conflicts via CLI over mTLS..."
CONF_OUT=$(run_client --db "${CLIENT_DB}" --db-key "${DB_KEY}" --key "${CLIENT_DEVICE_KEY}" conflicts \
    --server-url "${SERVER_URL}" --ca-cert "${CA_CERT}" --client-cert "${CLIENT_CERT}" --client-key "${CLIENT_KEY}")
echo "${CONF_OUT}"
echo "${CONF_OUT}" | grep -q "${E2E_ENTITY_1}"

echo "============================================================"
echo " All End-to-End checks passed successfully (mTLS + SQLCipher)!"
echo "============================================================"
