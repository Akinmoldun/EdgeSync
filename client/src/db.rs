use crate::clock::Relation;
use crate::models::{EntityState, Event, VectorClock};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Entity not found: {0}")]
    NotFound(String),
    #[error("Database encryption key required: provide --db-key <key> or set EDGESYNC_DB_KEY")]
    MissingKey,
    #[error("Invalid database encryption key or corrupted database: {0}")]
    InvalidKey(String),
}

pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Opens or creates an encrypted SQLite database file with SQLCipher.
    pub fn open<P: AsRef<Path>>(path: P, key: &str) -> Result<Self, DbError> {
        if key.trim().is_empty() {
            return Err(DbError::MissingKey);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "key", key)?;

        // Verify key by querying sqlite_master (forces SQLCipher to decrypt page 1)
        if let Err(e) = conn.query_row("SELECT count(*) FROM sqlite_master;", [], |row| {
            row.get::<_, i64>(0)
        }) {
            return Err(DbError::InvalidKey(e.to_string()));
        }

        let storage = Self { conn };
        storage.init_tables()?;
        Ok(storage)
    }

    /// Initializes in-memory encrypted SQLite database for testing.
    pub fn in_memory(key: &str) -> Result<Self, DbError> {
        if key.trim().is_empty() {
            return Err(DbError::MissingKey);
        }
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "key", key)?;
        let storage = Self { conn };
        storage.init_tables()?;
        Ok(storage)
    }

    /// Creates tables according to §6.1.
    pub fn init_tables(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                vector_clock TEXT NOT NULL,
                signature BLOB NOT NULL,
                synced INTEGER DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS outbox (
                event_id TEXT PRIMARY KEY REFERENCES events(id),
                attempts INTEGER DEFAULT 0,
                last_error TEXT
            );

            CREATE TABLE IF NOT EXISTS entity_state (
                entity_type TEXT,
                entity_id TEXT,
                state TEXT NOT NULL,
                vector_clock TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (entity_type, entity_id)
            );

            CREATE TABLE IF NOT EXISTS sync_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS conflicts (
                id TEXT PRIMARY KEY,
                entity_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                local_clock TEXT NOT NULL,
                remote_clock TEXT NOT NULL,
                remote_event_id TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Queries the current vector clock for an entity.
    /// If no entity state exists, returns an empty vector clock.
    pub fn get_entity_clock(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<VectorClock, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT vector_clock FROM entity_state WHERE entity_type = ?1 AND entity_id = ?2",
        )?;
        let clock_str: Option<String> = stmt
            .query_row(params![entity_type, entity_id], |row| row.get(0))
            .optional()?;

        match clock_str {
            Some(s) => {
                let clock: VectorClock = serde_json::from_str(&s)?;
                Ok(clock)
            }
            None => Ok(VectorClock::new()),
        }
    }

    /// Computes next vector clock for a mutation originating on local device.
    /// Starts at 1 if no previous counter exists for this device.
    pub fn next_vector_clock(
        &self,
        entity_type: &str,
        entity_id: &str,
        device_id: Uuid,
    ) -> Result<VectorClock, DbError> {
        let mut clock = self.get_entity_clock(entity_type, entity_id)?;
        let dev_key = device_id.to_string();
        clock.increment(&dev_key);
        Ok(clock)
    }

    /// Records an event locally, enqueues it in the outbox, and updates entity state atomically.
    pub fn append_event_and_update_state(
        &mut self,
        event: &Event,
        new_state: &serde_json::Value,
    ) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;

        let payload_json = serde_json::to_string(&event.payload)?;
        let clock_json = serde_json::to_string(&event.vector_clock)?;
        let state_json = serde_json::to_string(new_state)?;
        let id_str = event.id.to_string();

        // 1. Insert into events ledger
        tx.execute(
            r#"
            INSERT INTO events (id, entity_type, entity_id, event_type, payload, vector_clock, signature, synced, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                id_str,
                event.entity_type,
                event.entity_id,
                event.event_type,
                payload_json,
                clock_json,
                event.signature,
                event.synced,
                event.created_at,
            ],
        )?;

        // 2. Enqueue into outbox
        tx.execute(
            r#"
            INSERT INTO outbox (event_id, attempts, last_error)
            VALUES (?1, 0, NULL)
            "#,
            params![id_str],
        )?;

        // 3. Update local materialized entity state
        tx.execute(
            r#"
            INSERT INTO entity_state (entity_type, entity_id, state, vector_clock, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT (entity_type, entity_id)
            DO UPDATE SET
                state = excluded.state,
                vector_clock = excluded.vector_clock,
                updated_at = excluded.updated_at
            "#,
            params![
                event.entity_type,
                event.entity_id,
                state_json,
                clock_json,
                event.created_at,
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Retrieves pending outbox events.
    pub fn get_outbox_events(&self) -> Result<Vec<Event>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT e.id, e.entity_type, e.entity_id, e.event_type, e.payload, e.vector_clock, e.signature, e.synced, e.created_at
            FROM events e
            JOIN outbox o ON e.id = o.event_id
            ORDER BY e.created_at ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let entity_type: String = row.get(1)?;
            let entity_id: String = row.get(2)?;
            let event_type: String = row.get(3)?;
            let payload_str: String = row.get(4)?;
            let clock_str: String = row.get(5)?;
            let signature: Vec<u8> = row.get(6)?;
            let synced: i64 = row.get(7)?;
            let created_at: String = row.get(8)?;

            Ok((
                id_str,
                entity_type,
                entity_id,
                event_type,
                payload_str,
                clock_str,
                signature,
                synced,
                created_at,
            ))
        })?;

        let mut events = Vec::new();
        for r in rows {
            let (
                id_str,
                entity_type,
                entity_id,
                event_type,
                payload_str,
                clock_str,
                signature,
                synced,
                created_at,
            ) = r?;

            let id = Uuid::parse_str(&id_str)
                .map_err(|e| DbError::NotFound(format!("Invalid UUID in database: {}", e)))?;
            let payload: serde_json::Value = serde_json::from_str(&payload_str)?;
            let vector_clock: VectorClock = serde_json::from_str(&clock_str)?;

            events.push(Event {
                id,
                entity_type,
                entity_id,
                event_type,
                payload,
                vector_clock,
                signature,
                synced,
                created_at,
            });
        }
        Ok(events)
    }

    /// Retrieves all events stored in the local SQLite ledger.
    pub fn get_all_events(&self) -> Result<Vec<Event>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, entity_type, entity_id, event_type, payload, vector_clock, signature, synced, created_at
            FROM events
            ORDER BY created_at ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let entity_type: String = row.get(1)?;
            let entity_id: String = row.get(2)?;
            let event_type: String = row.get(3)?;
            let payload_str: String = row.get(4)?;
            let clock_str: String = row.get(5)?;
            let signature: Vec<u8> = row.get(6)?;
            let synced: i64 = row.get(7)?;
            let created_at: String = row.get(8)?;

            Ok((
                id_str,
                entity_type,
                entity_id,
                event_type,
                payload_str,
                clock_str,
                signature,
                synced,
                created_at,
            ))
        })?;

        let mut events = Vec::new();
        for r in rows {
            let (
                id_str,
                entity_type,
                entity_id,
                event_type,
                payload_str,
                clock_str,
                signature,
                synced,
                created_at,
            ) = r?;

            let id = Uuid::parse_str(&id_str)
                .map_err(|e| DbError::NotFound(format!("Invalid UUID in database: {}", e)))?;
            let payload: serde_json::Value = serde_json::from_str(&payload_str)?;
            let vector_clock: VectorClock = serde_json::from_str(&clock_str)?;

            events.push(Event {
                id,
                entity_type,
                entity_id,
                event_type,
                payload,
                vector_clock,
                signature,
                synced,
                created_at,
            });
        }
        Ok(events)
    }

    /// Retrieves the current materialized state for an entity.
    pub fn get_entity_state(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Option<EntityState>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT entity_type, entity_id, state, vector_clock, updated_at
            FROM entity_state
            WHERE entity_type = ?1 AND entity_id = ?2
            "#,
        )?;

        let result = stmt
            .query_row(params![entity_type, entity_id], |row| {
                let et: String = row.get(0)?;
                let ei: String = row.get(1)?;
                let state_str: String = row.get(2)?;
                let clock_str: String = row.get(3)?;
                let updated_at: String = row.get(4)?;
                Ok((et, ei, state_str, clock_str, updated_at))
            })
            .optional()?;

        match result {
            Some((et, ei, state_str, clock_str, updated_at)) => {
                let state: serde_json::Value = serde_json::from_str(&state_str)?;
                let vector_clock: VectorClock = serde_json::from_str(&clock_str)?;
                Ok(Some(EntityState {
                    entity_type: et,
                    entity_id: ei,
                    state,
                    vector_clock,
                    updated_at,
                }))
            }
            None => Ok(None),
        }
    }

    /// Marks an event as synced and removes it from the outbox.
    pub fn mark_synced(&mut self, event_id: Uuid) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        let id_str = event_id.to_string();

        tx.execute(
            "UPDATE events SET synced = 1 WHERE id = ?1",
            params![id_str],
        )?;
        tx.execute("DELETE FROM outbox WHERE event_id = ?1", params![id_str])?;

        tx.commit()?;
        Ok(())
    }

    /// Records a push failure attempt and error message for an outbox event.
    pub fn record_outbox_failure(&mut self, event_id: Uuid, error: &str) -> Result<(), DbError> {
        let id_str = event_id.to_string();
        self.conn.execute(
            r#"
            UPDATE outbox
            SET attempts = attempts + 1, last_error = ?2
            WHERE event_id = ?1
            "#,
            params![id_str, error],
        )?;
        Ok(())
    }

    /// Checks whether an event already exists in the local SQLite ledger.
    pub fn event_exists(&self, id: Uuid) -> Result<bool, DbError> {
        let mut stmt = self.conn.prepare("SELECT 1 FROM events WHERE id = ?1")?;
        let exists = stmt.exists(params![id.to_string()])?;
        Ok(exists)
    }

    /// Retrieves the last recorded server sequence number from sync metadata.
    pub fn get_last_server_seq(&self) -> Result<i64, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_metadata WHERE key = 'last_server_seq'")?;
        let val: Option<String> = stmt.query_row([], |row| row.get(0)).optional()?;
        match val {
            Some(s) => Ok(s.parse::<i64>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Updates the last recorded server sequence number in sync metadata.
    pub fn set_last_server_seq(&mut self, seq: i64) -> Result<(), DbError> {
        self.conn.execute(
            r#"
            INSERT INTO sync_metadata (key, value)
            VALUES ('last_server_seq', ?1)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![seq.to_string()],
        )?;
        Ok(())
    }

    /// Applies a remote event received from the sync server to the local database,
    /// enforcing strict Vector Clock causal checks.
    pub fn apply_pulled_event(&mut self, event: &Event) -> Result<ApplyOutcome, DbError> {
        let tx = self.conn.transaction()?;

        // 1. Idempotency check: if event already exists in ledger, ignore
        let mut exists_stmt = tx.prepare("SELECT 1 FROM events WHERE id = ?1")?;
        if exists_stmt.exists(params![event.id.to_string()])? {
            return Ok(ApplyOutcome::Ignored);
        }
        drop(exists_stmt);

        // 2. Insert event into ledger with synced = 1
        let payload_json = serde_json::to_string(&event.payload)?;
        let clock_json = serde_json::to_string(&event.vector_clock)?;
        let id_str = event.id.to_string();

        tx.execute(
            r#"
            INSERT INTO events (id, entity_type, entity_id, event_type, payload, vector_clock, signature, synced, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)
            "#,
            params![
                id_str,
                event.entity_type,
                event.entity_id,
                event.event_type,
                payload_json,
                clock_json,
                event.signature,
                event.created_at,
            ],
        )?;

        // 3. Compare with local entity_state
        let mut state_stmt = tx.prepare(
            "SELECT state, vector_clock, updated_at FROM entity_state WHERE entity_type = ?1 AND entity_id = ?2",
        )?;
        let existing = state_stmt
            .query_row(params![event.entity_type, event.entity_id], |row| {
                let state_str: String = row.get(0)?;
                let clock_str: String = row.get(1)?;
                let updated_at: String = row.get(2)?;
                Ok((state_str, clock_str, updated_at))
            })
            .optional()?;
        drop(state_stmt);

        let outcome = match existing {
            None => {
                // Dominates empty state: apply directly
                tx.execute(
                    r#"
                    INSERT INTO entity_state (entity_type, entity_id, state, vector_clock, updated_at)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        event.entity_type,
                        event.entity_id,
                        payload_json,
                        clock_json,
                        event.created_at,
                    ],
                )?;
                ApplyOutcome::Applied
            }
            Some((_existing_state_str, existing_clock_str, _)) => {
                let existing_clock: VectorClock = serde_json::from_str(&existing_clock_str)?;
                let relation = event.vector_clock.compare(&existing_clock);

                match relation {
                    Relation::Dominates => {
                        // Incoming event is causally newer: apply and merge clocks
                        let merged_clock = event.vector_clock.merge(&existing_clock);
                        let merged_clock_json = serde_json::to_string(&merged_clock)?;

                        tx.execute(
                            r#"
                            UPDATE entity_state
                            SET state = ?3, vector_clock = ?4, updated_at = ?5
                            WHERE entity_type = ?1 AND entity_id = ?2
                            "#,
                            params![
                                event.entity_type,
                                event.entity_id,
                                payload_json,
                                merged_clock_json,
                                event.created_at,
                            ],
                        )?;
                        ApplyOutcome::Applied
                    }
                    Relation::Dominated | Relation::Equal => {
                        // Incoming event is causally older or equal: do not update entity_state
                        ApplyOutcome::Ignored
                    }
                    Relation::Concurrent => {
                        // Hard ban: Do not overwrite local un-synced changes without checking vector clock!
                        // Concurrent conflict: Record in local conflicts table
                        let conflict_id = Uuid::new_v4().to_string();
                        tx.execute(
                            r#"
                            INSERT INTO conflicts (id, entity_type, entity_id, local_clock, remote_clock, remote_event_id, status, created_at)
                            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'unresolved', ?7)
                            "#,
                            params![
                                conflict_id,
                                event.entity_type,
                                event.entity_id,
                                existing_clock_str,
                                clock_json,
                                id_str,
                                event.created_at,
                            ],
                        )?;
                        ApplyOutcome::Conflict
                    }
                }
            }
        };

        tx.commit()?;
        Ok(outcome)
    }

    /// Queries all unresolved conflicts stored in local SQLite database.
    pub fn get_unresolved_conflicts(&self) -> Result<Vec<LocalConflict>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, entity_type, entity_id, local_clock, remote_clock, remote_event_id, status, created_at
            FROM conflicts
            WHERE status = 'unresolved'
            ORDER BY created_at ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let entity_type: String = row.get(1)?;
            let entity_id: String = row.get(2)?;
            let local_clock_str: String = row.get(3)?;
            let remote_clock_str: String = row.get(4)?;
            let remote_event_id: String = row.get(5)?;
            let status: String = row.get(6)?;
            let created_at: String = row.get(7)?;
            Ok((
                id,
                entity_type,
                entity_id,
                local_clock_str,
                remote_clock_str,
                remote_event_id,
                status,
                created_at,
            ))
        })?;

        let mut conflicts = Vec::new();
        for r in rows {
            let (
                id,
                entity_type,
                entity_id,
                local_clock_str,
                remote_clock_str,
                remote_event_id,
                status,
                created_at,
            ) = r?;
            let local_clock: VectorClock = serde_json::from_str(&local_clock_str)?;
            let remote_clock: VectorClock = serde_json::from_str(&remote_clock_str)?;
            conflicts.push(LocalConflict {
                id,
                entity_type,
                entity_id,
                local_clock,
                remote_clock,
                remote_event_id,
                status,
                created_at,
            });
        }
        Ok(conflicts)
    }

    /// Applies a resolved state to local SQLite, updating entity_state and marking matching conflicts resolved.
    pub fn apply_resolved_state(
        &mut self,
        entity_type: &str,
        entity_id: &str,
        state: &serde_json::Value,
        vector_clock: &VectorClock,
        updated_at: &str,
        conflict_id: Option<&str>,
    ) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        let state_json = serde_json::to_string(state)?;
        let clock_json = serde_json::to_string(vector_clock)?;

        tx.execute(
            r#"
            INSERT INTO entity_state (entity_type, entity_id, state, vector_clock, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(entity_type, entity_id) DO UPDATE SET
                state = excluded.state,
                vector_clock = excluded.vector_clock,
                updated_at = excluded.updated_at
            "#,
            params![entity_type, entity_id, state_json, clock_json, updated_at],
        )?;

        if let Some(cid) = conflict_id {
            tx.execute(
                "UPDATE conflicts SET status = 'resolved' WHERE id = ?1",
                params![cid],
            )?;
        } else {
            tx.execute(
                "UPDATE conflicts SET status = 'resolved' WHERE entity_type = ?1 AND entity_id = ?2",
                params![entity_type, entity_id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }
}

/// Local conflict record stored in SQLite when pulled event is concurrent with local entity state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalConflict {
    pub id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub local_clock: VectorClock,
    pub remote_clock: VectorClock,
    pub remote_event_id: String,
    pub status: String,
    pub created_at: String,
}

/// Result outcome of applying a pulled remote event against local state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Ignored,
    Conflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_event_insertion_and_state() {
        let mut storage =
            Storage::in_memory("test-encryption-key").expect("initialize in-memory db");
        let device_id = Uuid::new_v4();
        let entity_type = "revenue";
        let entity_id = "rev-unit-001";

        // Initial vector clock should be {"<device_id>": 1}
        let clock1 = storage
            .next_vector_clock(entity_type, entity_id, device_id)
            .expect("next clock 1");
        assert_eq!(clock1.get(&device_id.to_string()), Some(&1));

        let event1 = Event {
            id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({ "amount_cents": 2500, "currency": "USD" }),
            vector_clock: clock1.clone(),
            signature: vec![1u8; 64],
            synced: 0,
            created_at: "2026-11-05T00:00:00Z".to_string(),
        };

        storage
            .append_event_and_update_state(&event1, &event1.payload)
            .expect("append event 1");

        // Verify outbox contains event1
        let outbox = storage.get_outbox_events().expect("get outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].id, event1.id);

        // Verify entity state
        let state = storage
            .get_entity_state(entity_type, entity_id)
            .expect("get state")
            .expect("state must exist");
        assert_eq!(state.vector_clock.get(&device_id.to_string()), Some(&1));
        assert_eq!(state.state["amount_cents"], 2500);

        // Subsequent vector clock should increment to 2
        let clock2 = storage
            .next_vector_clock(entity_type, entity_id, device_id)
            .expect("next clock 2");
        assert_eq!(clock2.get(&device_id.to_string()), Some(&2));

        // Mark synced removes from outbox
        storage.mark_synced(event1.id).expect("mark synced");
        let outbox_empty = storage.get_outbox_events().expect("get empty outbox");
        assert_eq!(outbox_empty.len(), 0);

        // Event remains in events ledger
        let all_events = storage.get_all_events().expect("all events");
        assert_eq!(all_events.len(), 1);
        assert_eq!(all_events[0].synced, 1);
    }

    #[test]
    fn test_conflict_tracking_and_resolution() {
        let mut storage =
            Storage::in_memory("test-encryption-key").expect("initialize in-memory db");
        let dev_a = Uuid::new_v4();
        let dev_b = Uuid::new_v4();
        let entity_type = "revenue";
        let entity_id = "rev-conflict-test";

        // 1. Initial state written by dev A: clock {"dev_a": 1}
        let mut clock_a = VectorClock::new();
        clock_a.set(dev_a.to_string(), 1);
        let event_a = Event {
            id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({ "amount_cents": 1000, "source": "A" }),
            vector_clock: clock_a.clone(),
            signature: vec![1u8; 64],
            synced: 1,
            created_at: "2026-11-05T00:00:00Z".to_string(),
        };
        storage
            .append_event_and_update_state(&event_a, &event_a.payload)
            .expect("record initial event");

        // 2. Incoming concurrent event from dev B: clock {"dev_b": 1}
        let mut clock_b = VectorClock::new();
        clock_b.set(dev_b.to_string(), 1);
        let event_b = Event {
            id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({ "amount_cents": 2000, "source": "B" }),
            vector_clock: clock_b.clone(),
            signature: vec![2u8; 64],
            synced: 1,
            created_at: "2026-11-05T01:00:00Z".to_string(),
        };

        let outcome = storage
            .apply_pulled_event(&event_b)
            .expect("apply pulled event");
        assert_eq!(outcome, ApplyOutcome::Conflict);

        // 3. Verify conflict is surfaced in local conflicts table
        let unresolved = storage
            .get_unresolved_conflicts()
            .expect("get unresolved conflicts");
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].entity_id, entity_id);
        assert_eq!(unresolved[0].status, "unresolved");

        // 4. Resolve the conflict: choose B's payload with merged clock
        let merged_clock = clock_a.merge(&clock_b);
        let winning_state = serde_json::json!({ "amount_cents": 2000, "source": "B" });
        storage
            .apply_resolved_state(
                entity_type,
                entity_id,
                &winning_state,
                &merged_clock,
                "2026-11-05T02:00:00Z",
                Some(&unresolved[0].id),
            )
            .expect("apply resolved state");

        // 5. Verify conflict is now resolved
        let unresolved_after = storage
            .get_unresolved_conflicts()
            .expect("get unresolved conflicts after");
        assert_eq!(unresolved_after.len(), 0);

        // 6. Verify entity state has winning payload and merged vector clock
        let state = storage
            .get_entity_state(entity_type, entity_id)
            .expect("get entity state")
            .expect("entity state exists");
        assert_eq!(state.state["amount_cents"], 2000);
        assert_eq!(state.vector_clock.get(&dev_a.to_string()), Some(&1));
        assert_eq!(state.vector_clock.get(&dev_b.to_string()), Some(&1));
    }

    #[test]
    fn test_sqlcipher_encryption_key_enforcement() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("encrypted.db");

        // 1. Missing or empty key fails
        let missing_err = match Storage::open(&db_path, "") {
            Err(e) => e,
            Ok(_) => panic!("expected missing key error"),
        };
        assert!(matches!(missing_err, DbError::MissingKey));

        // 2. Create DB with correct key
        {
            let mut storage =
                Storage::open(&db_path, "correct-secret-123").expect("create encrypted db");
            let event = Event {
                id: Uuid::new_v4(),
                entity_type: "revenue".to_string(),
                entity_id: "rev-sec-01".to_string(),
                event_type: "revenue.recorded".to_string(),
                payload: serde_json::json!({ "amount_cents": 5000 }),
                vector_clock: VectorClock::new(),
                signature: vec![0u8; 64],
                synced: 0,
                created_at: "2026-11-08T00:00:00Z".to_string(),
            };
            storage
                .append_event_and_update_state(&event, &event.payload)
                .expect("append event");
        }

        // 3. Opening with wrong key fails!
        let wrong_err = Storage::open(&db_path, "wrong-password-456");
        assert!(wrong_err.is_err(), "Opening with wrong key must fail");

        // 4. Opening with correct key succeeds and can read data
        let storage =
            Storage::open(&db_path, "correct-secret-123").expect("reopen with correct key");
        let events = storage.get_all_events().expect("read events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].entity_id, "rev-sec-01");
    }
}
