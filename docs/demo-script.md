# EdgeSync — 2-Minute Demo Video Recording Script

This script provides exact commands, timing, and narration cues for recording a concise, high-impact 2-minute demonstration video of **EdgeSync**.

---

## Video Setup & Prerequisites

- **Terminal Setup:** Two terminal panes side-by-side or stacked:
  - **Pane 1 (Left / Top):** Edge Client CLI (`client/`).
  - **Pane 2 (Right / Bottom):** Go Sync Server logs & PostgreSQL monitor.
- **Environment Preparation:**
  ```powershell
  # Ensure PostgreSQL is running
  docker compose -f docker/docker-compose.yml up -d

  # Build Rust client binary
  cd client && cargo build && cd ..

  # Define shortcuts for recording
  $CLIENT = ".\client\target\x86_64-pc-windows-gnu\debug\edgesync.exe"
  $DB_KEY = "demo-vault-passphrase"
  ```

---

## Demo Script Walkthrough

### 0:00 – 0:20 | Introduction & Device Initialization
**Narration:**
> *"Welcome! This is EdgeSync: an offline-first, DDIL-resilient synchronization engine designed for edge environments where connectivity is intermittent or denied. On the edge, we store all mutations locally in a 256-bit AES SQLCipher encrypted database, signed cryptographically with Ed25519."*

**Action (Pane 1): Initialize Edge Identity**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key init-device
```

**Expected Output:**
```text
EdgeSync Device Identity Initialized:
  Device ID:        c82f91a2-7104-4b4e-9d21-f3b185d263a1
  Public Key (hex): 3b6a9e81...
  Key file path:    demo.key
```

**Action (Pane 2): Register Device in Central Server**
```powershell
docker exec -it edgesync-postgres psql -U postgres -d edgesync -c "INSERT INTO devices (id, public_key) VALUES ('c82f91a2-7104-4b4e-9d21-f3b185d263a1', decode('3b6a9e81...', 'hex')) ON CONFLICT DO NOTHING;"
```

---

### 0:20 – 0:45 | Step 1: Local Offline Event Generation
**Narration:**
> *"Operators can record critical transactions without any network connection. All amounts are strictly modeled as 64-bit integer minor currency units to eliminate floating-point rounding errors."*

**Action (Pane 1): Add Local Offline Revenue Transactions**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key add-revenue --amount 5000 --description "Field Fuel Purchase" --entity-id rev-001
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key add-revenue --amount 2500 --description "Field Rations" --entity-id rev-002
```

**Action (Pane 1): Inspect Outbox**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key outbox
```

**Expected Output:**
```text
Pending Outbox Events (2):
  [1] Event ID: ... | Entity: revenue/rev-001 | Amount: $50.00 | Clock: {"c82f91a2...": 1}
  [2] Event ID: ... | Entity: revenue/rev-002 | Amount: $25.00 | Clock: {"c82f91a2...": 2}
```

---

### 0:45 – 1:10 | Step 2: The DDIL Blackout (Simulating Network Drop)
**Narration:**
> *"Now let's simulate a total network partition or server blackout. Even with the central server offline, field operations continue uninterrupted."*

**Action (Pane 2): Kill the Sync Server**
```powershell
# Stop Go server process (Ctrl+C in terminal or kill command)
Stop-Process -Name api -Force -ErrorAction SilentlyContinue
```

**Action (Pane 1): Generate Offline Work During Blackout**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key add-revenue --amount 12000 --description "Tactical Hardware Replacement" --entity-id rev-003
```

**Action (Pane 1): Attempt Sync During Blackout**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key push --server-url http://localhost:8080
```

**Expected Output:**
> Network failure surfaced cleanly; zero data lost; outbox retains all 3 pending events safely encrypted on disk.

---

### 1:10 – 1:35 | Step 3: Reconnection & Zero-Data-Loss Sync
**Narration:**
> *"Once network connectivity is re-established, the edge client drains its outbox in a single batch push over mutual TLS or HTTPS."*

**Action (Pane 2): Restart the Go Sync Server**
```powershell
cd server && go run ./cmd/api
```

**Action (Pane 1): Push Accumulated Outbox**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key push --server-url http://localhost:8080
```

**Expected Output:**
```text
Sync push summary:
  Accepted:   3
  Ignored:    0
  Conflicts:  0
  Rejected:   0
```

**Action (Pane 2): Query Central PostgreSQL Ledger**
```powershell
docker exec -it edgesync-postgres psql -U postgres -d edgesync -c "SELECT entity_id, event_type, payload->>'amount_cents' as cents, vector_clock FROM events WHERE device_id = 'c82f91a2-7104-4b4e-9d21-f3b185d263a1';"
```

**Expected Output:**
> Exactly 3 events confirmed in PostgreSQL with incremental vector clocks. Client outbox is now empty (`0 pending`).

---

### 1:35 – 2:00 | Step 4: Concurrent Conflict Detection & Resolution
**Narration:**
> *"What happens when two edge nodes modify the same entity while disconnected? Instead of silent overwrites or relying on flawed wall-clock timestamps, EdgeSync detects concurrent vector clocks and surfaces conflicts for deterministic resolution."*

**Action (Pane 1): Local Modification on rev-001**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key add-revenue --amount 7500 --description "Client Price Update" --entity-id rev-001
```

**Action (Pane 2): Concurrent Remote Mutation Injected Directly**
```powershell
docker exec -it edgesync-postgres psql -U postgres -d edgesync -c "INSERT INTO devices (id, public_key) VALUES ('00000000-0000-0000-0000-000000000002', decode('0000000000000000000000000000000000000000000000000000000000000000', 'hex')) ON CONFLICT DO NOTHING;"
docker exec -it edgesync-postgres psql -U postgres -d edgesync -c "INSERT INTO events (id, device_id, entity_type, entity_id, event_type, payload, vector_clock, signature) VALUES (gen_random_uuid(), '00000000-0000-0000-0000-000000000002', 'revenue', 'rev-001', 'revenue.recorded', '{\"amount_cents\": 9500}', '{\"00000000-0000-0000-0000-000000000002\": 1}', decode('00' || repeat('00', 63), 'hex'));"
```

**Action (Pane 1): Pull & Surface Conflict**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key pull --server-url http://localhost:8080
```

**Expected Output:**
```text
Sync pull summary:
  Applied:    0
  Ignored:    0
  Conflicts:  1
```

**Action (Pane 1): List Conflicts**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key conflicts --server-url http://localhost:8080
```

**Action (Pane 1): Resolve Conflict via CLI**
```powershell
& $CLIENT --db demo.db --db-key $DB_KEY --key demo.key resolve-conflict --conflict-id <CONFLICT_ID> --resolution keep_mine --server-url http://localhost:8080
```

**Closing Narration:**
> *"The conflict is resolved, merging vector clocks from both nodes so that future edits cleanly dominate. Cryptographic verification, zero wall-clock dependencies, and zero data loss in DDIL environments. That is EdgeSync."*
