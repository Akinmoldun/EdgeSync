# Building a DDIL-Resilient Sync Engine in Rust and Go

In distributed systems, the network is often treated as a reliable, high-bandwidth abstraction. However, in tactical defense, maritime logistics, disaster response, and remote aerospace environments, connectivity is fundamentally **DDIL: Denied, Disrupted, Intermittent, and Limited**.

When field nodes operate in complete isolation for hours or days, traditional distributed patterns—such as two-phase commits, synchronous Raft consensus, or naive Last-Write-Wins (LWW) reconciliation—break down. If an edge node cannot talk to a cluster leader, either all local operations halt, or concurrent writes silently overwrite critical data upon reconnection.

To solve this, I designed and built **EdgeSync**: an offline-first, cryptographically verifiable synchronization engine combining a hardened Rust client at the tactical edge with a high-throughput Go central server backed by PostgreSQL 16.

Here is how EdgeSync solves the core challenges of distributed synchronization in DDIL enclaves.

---

## 1. Offline-First Event Sourcing with SQLCipher

In a DDIL environment, the edge cannot depend on remote validation. All local mutations must be committed immediately and durably.

EdgeSync treats every business operation (such as recording a financial transaction or inventory reallocation) as an immutable, append-only event. The Rust client writes events to an internal SQLite event ledger and enqueues them into an **outbox queue** inside the same ACID transaction. 

Because physical edge devices can be stolen, captured, or inspected, at-rest plaintext storage is unacceptable. We encrypted the SQLite engine using **SQLCipher (256-bit AES-GCM)**. The database key is supplied via `--db-key` or environment variables, and the client runs a fail-fast probe against page 1 (`sqlite_master`) on startup. If the key is missing or invalid, the client immediately terminates with an `InvalidKey` error, refusing to operate.

```rust
// Open encrypted database with fail-fast validation
pub fn open<P: AsRef<Path>>(path: P, key: &str) -> Result<Self, DbError> {
    if key.trim().is_empty() {
        return Err(DbError::MissingKey);
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "key", key)?;
    // Force cryptographic decryption of page 1
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|e| DbError::InvalidKey(e.to_string()))?;
    Ok(Storage { conn })
}
```

---

## 2. Cryptographic Device Identity with Ed25519

In untrusted field topologies, the server cannot trust network addresses or IP headers. Every edge device generates an **Ed25519** keypair during initialization. The private key remains in local secure storage, while the public key is registered with the central server.

Before any event leaves the outbox, the Rust client computes a deterministic, canonical byte representation of the event payload and signs it using its Ed25519 private key:

$$\text{Signature} = \text{Sign}_{K_{\text{priv}}}(\text{CanonicalEventBytes})$$

When the Go sync server receives a batch push (`POST /sync/push`), it retrieves the registered public key for `device_id` and verifies every signature before acquiring a database lock. If a payload was altered in transit or issued by an unregistered node, it is rejected immediately. This guarantees non-repudiation and end-to-end event integrity.

---

## 3. Causal Consistency with Vector Clocks (No Wall-Clock Timestamps)

One of the most dangerous anti-patterns in distributed synchronization is relying on wall-clock timestamps (like NTP or UTC timestamps) to determine event ordering. In DDIL scenarios, GPS signals may be jammed, and hardware RTC clocks drift significantly over weeks of isolation. If device A's clock runs 5 minutes fast, its events would blindly overwrite device B's newer changes under Last-Write-Wins.

EdgeSync completely bans wall-clock ordering. Instead, causality is tracked via **Vector Clocks**: a logical mapping of device IDs to monotonic counter sequences:

$$V = \{ \text{node}_1: c_1, \text{node}_2: c_2, \dots, \text{node}_n: c_n \}$$

Whenever an edge device produces a mutation, it increments its own entry in the vector clock:

$$V_{\text{local}}[\text{self}] \leftarrow V_{\text{local}}[\text{self}] + 1$$

When events arrive at the Go server, the server compares the incoming event's clock ($V_A$) against the latest known clock for that entity ($V_B$):

1. **Dominates ($V_A > V_B$):** For all keys $k$, $V_A[k] \ge V_B[k]$, and $\exists k$ such that $V_A[k] > V_B[k]$. Event $A$ causally succeeds $B$ and is applied to the materialized entity state.
2. **Dominated ($V_A \le V_B$):** Event $A$ is an old or duplicate event and is safely ignored (idempotent).
3. **Concurrent ($V_A \parallel V_B$):** Neither clock dominates the other. For example, $V_A = \{ \text{dev}_1: 2 \}$ and $V_B = \{ \text{dev}_2: 1 \}$. 

```go
func Compare(a, b VectorClock) DominanceRelation {
    aHasGreater := false
    bHasGreater := false

    allKeys := make(map[string]struct{})
    for k := range a { allKeys[k] = struct{}{} }
    for k := range b { allKeys[k] = struct{}{} }

    for k := range allKeys {
        va := a[k]
        vb := b[k]
        if va > vb { aHasGreater = true }
        if vb > va { bHasGreater = true }
    }

    if aHasGreater && !bHasGreater { return Dominates }
    if bHasGreater && !aHasGreater { return DominatedBy }
    if !aHasGreater && !bHasGreater { return Equal }
    return Concurrent
}
```

---

## 4. Conflict Surfacing and Clock Merging

Under concurrent modification, EdgeSync **refuses to silently overwrite either state**. Instead, the Go server flags the conflict in the PostgreSQL `conflicts` table and returns an HTTP `409 Conflict`.

Operators can list and inspect pending conflicts via the CLI (`edgesync conflicts`). When resolving a conflict (e.g., choosing `keep_mine`, `accept_theirs`, or supplying a manual resolution value), EdgeSync applies a crucial architectural step: **Vector Clock Merging**.

$$\forall k \in (V_A \cup V_B), \quad V_{\text{resolved}}[k] = \max(V_A[k], V_B[k]) + 1_{\text{local}}$$

By merging the clocks from both divergent branches, the resolution event causally dominates both prior histories. This prevents "phantom conflicts" where subsequent edits from either node might otherwise trigger recurring conflict errors.

---

## 5. Defense-in-Depth: Full mTLS and Chaos Verification

Security in EdgeSync extends to the transport layer. For private enclaves, the Go server requires **Mutual TLS (mTLS)** via `tls.RequireAndVerifyClientCert`, requiring clients to present a valid X.509 certificate signed by the root CA. For cloud deployments on Fly.io, the server includes a graceful fallback to standard HTTP when public edge proxies handle TLS termination.

To prove resilience under real-world DDIL warfare, we developed a 10-phase chaos test harness (`scripts/chaos_test.ps1` and `.sh`). The harness starts the system, generates 50 events, forcibly kills the server process, generates 50 additional events in complete blackout, restarts the server, pushes the batch, and verifies with mathematical certainty that **all 100 events synced with zero errors and zero data loss**.

---

## Conclusion

Building resilient software for contested environments requires shedding fragile assumptions about network availability and clock synchronization. By pairing **Rust's memory safety and SQLCipher** on the edge with **Go's high-concurrency event ingestion**, and anchoring the entire architecture in **Vector Clocks and Ed25519 cryptography**, EdgeSync proves that true offline-first consistency is possible even under total network collapse.

Check out the full open-source implementation on GitHub: [https://github.com/Akinmoldun/EdgeSync](https://github.com/Akinmoldun/EdgeSync).
