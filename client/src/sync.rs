use crate::crypto::{hex_encode, parse_signature};
use crate::db::{ApplyOutcome, DbError, Storage};
use crate::models::{EntityState, Event, VectorClock};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Database error during sync: {0}")]
    Database(#[from] DbError),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Bad request from server: {0}")]
    BadRequest(String),
    #[error("Not found on server: {0}")]
    NotFound(String),
    #[error("Server error: {0}")]
    ServerError(String),
    #[error(
        "Unsigned event detected: {0}. Strict security policy prohibits sending unsigned events."
    )]
    UnsignedEvent(Uuid),
    #[error("Invalid signature on pulled event: {0}")]
    InvalidSignature(String),
    #[error("JSON serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("TLS configuration error: {0}")]
    Tls(String),
}

/// PushEvent represents a single event formatted for the server POST /sync/push endpoint (§7.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushEvent {
    pub id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub vector_clock: VectorClock,
    pub signature: String, // hex encoded 64-byte Ed25519 signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl From<&Event> for PushEvent {
    fn from(ev: &Event) -> Self {
        PushEvent {
            id: ev.id,
            entity_type: ev.entity_type.clone(),
            entity_id: ev.entity_id.clone(),
            event_type: ev.event_type.clone(),
            payload: ev.payload.clone(),
            vector_clock: ev.vector_clock.clone(),
            signature: hex_encode(&ev.signature),
            created_at: Some(ev.created_at.clone()),
        }
    }
}

/// PushRequest represents the payload sent to POST /sync/push (§7.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    pub device_id: Uuid,
    pub events: Vec<PushEvent>,
}

/// ConflictSummary details a concurrent update detected by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictSummary {
    pub id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub event_a_id: Uuid,
    pub event_b_id: Uuid,
}

/// PushResponse conveys acceptance, idempotency, conflict, or rejection details from server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PushResponse {
    #[serde(default)]
    pub accepted: Vec<Uuid>,
    #[serde(default)]
    pub ignored: Vec<Uuid>,
    #[serde(default)]
    pub conflicts: Vec<ConflictSummary>,
    #[serde(default)]
    pub rejected: Vec<Uuid>,
}

/// Summary of a push sync execution.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushResult {
    pub accepted_count: usize,
    pub ignored_count: usize,
    pub conflict_count: usize,
    pub rejected_count: usize,
    pub conflicts: Vec<ConflictSummary>,
}

/// PulledEvent represents an event received from GET /sync/pull (§7.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulledEvent {
    pub id: Uuid,
    pub device_id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub vector_clock: VectorClock,
    pub signature: serde_json::Value,
    #[serde(default)]
    pub server_seq: Option<i64>,
    pub created_at: String,
}

impl TryFrom<PulledEvent> for Event {
    type Error = SyncError;

    fn try_from(pe: PulledEvent) -> Result<Self, Self::Error> {
        let sig_bytes = parse_signature(&pe.signature).map_err(SyncError::InvalidSignature)?;
        Ok(Event {
            id: pe.id,
            entity_type: pe.entity_type,
            entity_id: pe.entity_id,
            event_type: pe.event_type,
            payload: pe.payload,
            vector_clock: pe.vector_clock,
            signature: sig_bytes,
            synced: 1,
            created_at: pe.created_at,
        })
    }
}

/// Summary of a pull sync execution.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PullResult {
    pub total_pulled: usize,
    pub applied_count: usize,
    pub ignored_count: usize,
    pub conflict_count: usize,
    pub last_server_seq: i64,
}

/// ServerConflict represents an unresolved conflict returned by GET /conflicts (§8 Week 7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConflict {
    pub id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub event_a_id: Uuid,
    pub event_b_id: Uuid,
    pub status: String,
    pub created_at: String,
}

/// ResolveConflictRequest specifies the human-selected winning event ID (§8 Week 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveConflictRequest {
    pub winner_event_id: Uuid,
}

/// ResolveConflictResponse conveys the updated entity state and resolution details from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveConflictResponse {
    pub status: String,
    pub conflict_id: Uuid,
    pub winner_event_id: Uuid,
    pub entity_state: EntityState,
}

/// mTLS certificate and key configuration (§8 Week 10).
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pub ca_cert_path: Option<String>,
    pub client_cert_path: Option<String>,
    pub client_key_path: Option<String>,
}

pub struct SyncClient {
    client: reqwest::Client,
    server_url: String,
}

impl SyncClient {
    /// Creates a SyncClient with a default 30s request timeout and 10s connect timeout (§7.2, §8).
    pub fn new(server_url: String) -> Result<Self, SyncError> {
        Self::with_timeout(server_url, Duration::from_secs(30))
    }

    /// Creates a SyncClient with a specified network timeout.
    pub fn with_timeout(server_url: String, timeout: Duration) -> Result<Self, SyncError> {
        Self::with_options(server_url, timeout, None)
    }

    /// Creates a SyncClient with timeout and optional mTLS configuration (§8 Week 10).
    pub fn with_options(
        server_url: String,
        timeout: Duration,
        tls_config: Option<TlsConfig>,
    ) -> Result<Self, SyncError> {
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(5).min(timeout));

        if let Some(tls) = tls_config {
            if let Some(ca_path) = tls.ca_cert_path {
                let ca_bytes = std::fs::read(&ca_path).map_err(|e| {
                    SyncError::Tls(format!(
                        "Failed to read CA certificate at {}: {}",
                        ca_path, e
                    ))
                })?;
                let ca_cert = reqwest::Certificate::from_pem(&ca_bytes).map_err(|e| {
                    SyncError::Tls(format!("Invalid CA PEM certificate at {}: {}", ca_path, e))
                })?;
                builder = builder.add_root_certificate(ca_cert);
            }

            if let (Some(cert_path), Some(key_path)) = (tls.client_cert_path, tls.client_key_path) {
                let cert_bytes = std::fs::read(&cert_path).map_err(|e| {
                    SyncError::Tls(format!(
                        "Failed to read client certificate at {}: {}",
                        cert_path, e
                    ))
                })?;
                let key_bytes = std::fs::read(&key_path).map_err(|e| {
                    SyncError::Tls(format!(
                        "Failed to read client private key at {}: {}",
                        key_path, e
                    ))
                })?;

                let mut combined_pem = cert_bytes;
                combined_pem.push(b'\n');
                combined_pem.extend_from_slice(&key_bytes);

                let identity = reqwest::Identity::from_pem(&combined_pem).map_err(|e| {
                    SyncError::Tls(format!(
                        "Failed to parse client identity from {} and {}: {}",
                        cert_path, key_path, e
                    ))
                })?;
                builder = builder.identity(identity);
            }
        }

        let client = builder.build().map_err(SyncError::Http)?;
        Ok(Self { client, server_url })
    }

    /// Verifies connectivity and mTLS authentication against server /healthz.
    pub async fn check_health(&self) -> Result<bool, SyncError> {
        let endpoint = format!("{}/healthz", self.server_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(SyncError::Http)?;
        Ok(resp.status().is_success())
    }

    /// Reads pending events from outbox, pushes them to server, and updates local state.
    pub async fn push_outbox(
        &self,
        storage: &mut Storage,
        device_id: Uuid,
    ) -> Result<PushResult, SyncError> {
        let outbox_events = storage.get_outbox_events()?;
        if outbox_events.is_empty() {
            return Ok(PushResult::default());
        }

        // Hard ban: Never send unsigned events
        for ev in &outbox_events {
            if ev.signature.is_empty() {
                return Err(SyncError::UnsignedEvent(ev.id));
            }
        }

        let push_events: Vec<PushEvent> = outbox_events.iter().map(PushEvent::from).collect();
        let push_req = PushRequest {
            device_id,
            events: push_events,
        };

        let endpoint = format!("{}/sync/push", self.server_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&endpoint)
            .json(&push_req)
            .send()
            .await
            .map_err(SyncError::Http)?;

        let status = resp.status();

        match status {
            StatusCode::OK => {
                let push_resp: PushResponse = resp.json().await.map_err(SyncError::Http)?;

                for id in &push_resp.accepted {
                    storage.mark_synced(*id)?;
                }
                for id in &push_resp.ignored {
                    storage.mark_synced(*id)?;
                }
                for id in &push_resp.rejected {
                    storage.record_outbox_failure(
                        *id,
                        "Rejected by server (invalid signature or malformed)",
                    )?;
                }

                Ok(PushResult {
                    accepted_count: push_resp.accepted.len(),
                    ignored_count: push_resp.ignored.len(),
                    conflict_count: 0,
                    rejected_count: push_resp.rejected.len(),
                    conflicts: vec![],
                })
            }
            StatusCode::CONFLICT => {
                let push_resp: PushResponse = resp.json().await.map_err(SyncError::Http)?;

                for id in &push_resp.accepted {
                    storage.mark_synced(*id)?;
                }
                for id in &push_resp.ignored {
                    storage.mark_synced(*id)?;
                }
                // Concurrently conflicting events were stored by server in conflicts table.
                // Mark synced locally so outbox is drained and not head-of-line blocked.
                for c in &push_resp.conflicts {
                    storage.mark_synced(c.event_b_id)?;
                }
                for id in &push_resp.rejected {
                    storage.record_outbox_failure(*id, "Rejected by server")?;
                }

                Ok(PushResult {
                    accepted_count: push_resp.accepted.len(),
                    ignored_count: push_resp.ignored.len(),
                    conflict_count: push_resp.conflicts.len(),
                    rejected_count: push_resp.rejected.len(),
                    conflicts: push_resp.conflicts,
                })
            }
            StatusCode::UNAUTHORIZED => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::Unauthorized(format!(
                    "Device not registered on server: {}",
                    body
                )))
            }
            StatusCode::BAD_REQUEST => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::BadRequest(format!(
                    "Server returned 400 Bad Request: {}",
                    body
                )))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::ServerError(format!(
                    "Server returned HTTP {}: {}",
                    s, body
                )))
            }
        }
    }

    /// Fetches events from server where server_seq > after_seq (§7.3).
    pub async fn pull_events(&self, after_seq: i64) -> Result<Vec<PulledEvent>, SyncError> {
        let endpoint = format!(
            "{}/sync/pull?after_seq={}",
            self.server_url.trim_end_matches('/'),
            after_seq
        );
        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(SyncError::Http)?;

        let status = resp.status();
        if status != StatusCode::OK {
            let body = resp.text().await.unwrap_or_default();
            return Err(SyncError::ServerError(format!(
                "Server returned HTTP {}: {}",
                status, body
            )));
        }

        let events: Vec<PulledEvent> = resp.json().await.map_err(SyncError::Http)?;
        Ok(events)
    }

    /// Pulls events from server after the locally tracked sequence number and applies them to local SQLite.
    pub async fn pull_and_apply(
        &self,
        storage: &mut Storage,
        override_after_seq: Option<i64>,
    ) -> Result<PullResult, SyncError> {
        let after_seq = match override_after_seq {
            Some(s) => s,
            None => storage.get_last_server_seq()?,
        };

        let pulled_events = self.pull_events(after_seq).await?;
        let mut applied_count = 0;
        let mut ignored_count = 0;
        let mut conflict_count = 0;
        let mut max_seq = after_seq;

        for pe in pulled_events {
            if let Some(seq) = pe.server_seq {
                if seq > max_seq {
                    max_seq = seq;
                }
            }

            let event = Event::try_from(pe)?;
            let outcome = storage.apply_pulled_event(&event)?;
            match outcome {
                ApplyOutcome::Applied => applied_count += 1,
                ApplyOutcome::Ignored => ignored_count += 1,
                ApplyOutcome::Conflict => conflict_count += 1,
            }
        }

        if max_seq > after_seq {
            storage.set_last_server_seq(max_seq)?;
        }

        Ok(PullResult {
            total_pulled: applied_count + ignored_count + conflict_count,
            applied_count,
            ignored_count,
            conflict_count,
            last_server_seq: max_seq,
        })
    }

    /// Fetches all unresolved conflicts from the server via GET /conflicts.
    pub async fn get_conflicts(&self) -> Result<Vec<ServerConflict>, SyncError> {
        let endpoint = format!("{}/conflicts", self.server_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&endpoint)
            .send()
            .await
            .map_err(SyncError::Http)?;

        let status = resp.status();
        if status != StatusCode::OK {
            let body = resp.text().await.unwrap_or_default();
            return Err(SyncError::ServerError(format!(
                "Server returned HTTP {}: {}",
                status, body
            )));
        }

        let conflicts: Vec<ServerConflict> = resp.json().await.map_err(SyncError::Http)?;
        Ok(conflicts)
    }

    /// Resolves a conflict on the server by specifying the winning event ID via POST /conflicts/{id}/resolve.
    pub async fn resolve_conflict(
        &self,
        conflict_id: Uuid,
        winner_event_id: Uuid,
    ) -> Result<ResolveConflictResponse, SyncError> {
        let endpoint = format!(
            "{}/conflicts/{}/resolve",
            self.server_url.trim_end_matches('/'),
            conflict_id
        );
        let req_body = ResolveConflictRequest { winner_event_id };

        let resp = self
            .client
            .post(&endpoint)
            .json(&req_body)
            .send()
            .await
            .map_err(SyncError::Http)?;

        let status = resp.status();
        match status {
            StatusCode::OK => {
                let resolved: ResolveConflictResponse =
                    resp.json().await.map_err(SyncError::Http)?;
                Ok(resolved)
            }
            StatusCode::BAD_REQUEST => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::BadRequest(body))
            }
            StatusCode::NOT_FOUND => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::NotFound(body))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(SyncError::ServerError(format!(
                    "Server returned HTTP {}: {}",
                    s, body
                )))
            }
        }
    }

    /// Resolves a conflict on the server and applies the resulting entity state to the local SQLite database.
    pub async fn resolve_and_apply(
        &self,
        storage: &mut Storage,
        conflict_id: Uuid,
        winner_event_id: Uuid,
    ) -> Result<ResolveConflictResponse, SyncError> {
        let resolution = self.resolve_conflict(conflict_id, winner_event_id).await?;

        storage.apply_resolved_state(
            &resolution.entity_state.entity_type,
            &resolution.entity_state.entity_id,
            &resolution.entity_state.state,
            &resolution.entity_state.vector_clock,
            &resolution.entity_state.updated_at,
            Some(&conflict_id.to_string()),
        )?;

        Ok(resolution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Keypair;
    use crate::models::RevenuePayload;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn start_mock_server(
        status_code: u16,
        response_body: String,
    ) -> (String, tokio::sync::oneshot::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tokio::select! {
                _ = &mut shutdown_rx => {}
                res = listener.accept() => {
                    if let Ok((mut socket, _)) = res {
                        let mut buffer = [0u8; 8192];
                        let n = socket.read(&mut buffer).await.unwrap_or(0);
                        let req_str = String::from_utf8_lossy(&buffer[..n]);

                        assert!(
                            req_str.starts_with("POST /sync/push")
                                || req_str.starts_with("GET /sync/pull")
                                || req_str.starts_with("GET /conflicts")
                                || req_str.starts_with("POST /conflicts/")
                        );

                        let status_line = match status_code {
                            200 => "HTTP/1.1 200 OK",
                            400 => "HTTP/1.1 400 Bad Request",
                            401 => "HTTP/1.1 401 Unauthorized",
                            404 => "HTTP/1.1 404 Not Found",
                            409 => "HTTP/1.1 409 Conflict",
                            _ => "HTTP/1.1 500 Internal Server Error",
                        };

                        let response = format!(
                            "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            status_line,
                            response_body.len(),
                            response_body
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.flush().await;
                    }
                }
            }
        });

        (format!("http://{}", addr), shutdown_tx)
    }

    #[tokio::test]
    async fn test_push_outbox_clears_on_200_ok() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let key_path = dir.path().join("test.key");

        let keypair = Keypair::load_or_generate(&key_path).expect("keypair");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let entity_id = "rev-test-sync-1";
        let entity_type = "revenue";

        let mut clock = VectorClock::new();
        clock.increment(&keypair.device_id.to_string());

        let event_id = Uuid::new_v4();
        let payload = RevenuePayload {
            amount_cents: 5000,
            currency: "USD".to_string(),
            description: Some("Outbox push test".to_string()),
        };
        let payload_json = serde_json::to_value(&payload).expect("payload to value");
        let compact_payload = serde_json::to_string(&payload_json).expect("compact payload");
        let sig = keypair.sign(format!("{}:{}", event_id, compact_payload).as_bytes());

        let event = Event {
            id: event_id,
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: payload_json.clone(),
            vector_clock: clock,
            signature: sig,
            synced: 0,
            created_at: "2026-11-06T12:00:00Z".to_string(),
        };

        storage
            .append_event_and_update_state(&event, &payload_json)
            .expect("save event");

        let outbox = storage.get_outbox_events().expect("get outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].id, event_id);

        let response_body = format!(
            r#"{{"accepted":["{}"],"ignored":[],"conflicts":[],"rejected":[]}}"#,
            event_id
        );
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .push_outbox(&mut storage, keypair.device_id)
            .await
            .expect("push outbox");

        assert_eq!(result.accepted_count, 1);
        assert_eq!(result.ignored_count, 0);
        assert_eq!(result.conflict_count, 0);

        // Verify outbox is drained
        let outbox_after = storage.get_outbox_events().expect("outbox after push");
        assert!(outbox_after.is_empty(), "Outbox must be empty after 200 OK");

        // Verify event marked synced = 1
        let all_events = storage.get_all_events().expect("all events");
        assert_eq!(all_events.len(), 1);
        assert_eq!(all_events[0].synced, 1);
    }

    #[tokio::test]
    async fn test_push_outbox_409_conflict() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_conflict.db");
        let key_path = dir.path().join("test_conflict.key");

        let keypair = Keypair::load_or_generate(&key_path).expect("keypair");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let entity_id = "rev-conflict-1";
        let entity_type = "revenue";

        let mut clock = VectorClock::new();
        clock.increment(&keypair.device_id.to_string());

        let event_id = Uuid::new_v4();
        let payload = RevenuePayload {
            amount_cents: 1200,
            currency: "USD".to_string(),
            description: Some("Conflict test".to_string()),
        };
        let payload_json = serde_json::to_value(&payload).expect("payload to value");
        let compact_payload = serde_json::to_string(&payload_json).expect("compact payload");
        let sig = keypair.sign(format!("{}:{}", event_id, compact_payload).as_bytes());

        let event = Event {
            id: event_id,
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: payload_json.clone(),
            vector_clock: clock,
            signature: sig,
            synced: 0,
            created_at: "2026-11-06T12:00:00Z".to_string(),
        };

        storage
            .append_event_and_update_state(&event, &payload_json)
            .expect("save event");

        let conflict_id = Uuid::new_v4();
        let other_event_id = Uuid::new_v4();
        let response_body = format!(
            r#"{{"accepted":[],"ignored":[],"conflicts":[{{"id":"{}","entity_type":"revenue","entity_id":"{}","event_a_id":"{}","event_b_id":"{}"}}],"rejected":[]}}"#,
            conflict_id, entity_id, other_event_id, event_id
        );
        let (server_url, _shutdown) = start_mock_server(409, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .push_outbox(&mut storage, keypair.device_id)
            .await
            .expect("push outbox with conflict");

        assert_eq!(result.conflict_count, 1);
        assert_eq!(result.conflicts[0].id, conflict_id);

        // Outbox drained so it does not block future events
        let outbox_after = storage.get_outbox_events().expect("outbox after push");
        assert!(outbox_after.is_empty());
    }

    #[tokio::test]
    async fn test_push_unsigned_event_rejected() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_unsigned.db");
        let key_path = dir.path().join("test_unsigned.key");

        let keypair = Keypair::load_or_generate(&key_path).expect("keypair");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let event = Event {
            id: Uuid::new_v4(),
            entity_type: "revenue".to_string(),
            entity_id: "rev-unsigned".to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 100}),
            vector_clock: VectorClock::new(),
            signature: vec![], // UNSIGNED!
            synced: 0,
            created_at: "2026-11-06T12:00:00Z".to_string(),
        };

        storage
            .append_event_and_update_state(&event, &serde_json::json!({"amount_cents": 100}))
            .expect("save event");

        let client = SyncClient::new("http://127.0.0.1:9999".to_string()).expect("client");
        let err = client
            .push_outbox(&mut storage, keypair.device_id)
            .await
            .expect_err("must reject unsigned event");

        match err {
            SyncError::UnsignedEvent(id) => assert_eq!(id, event.id),
            other => panic!("Expected UnsignedEvent error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_push_empty_outbox() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_empty.db");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let client = SyncClient::new("http://127.0.0.1:9999".to_string()).expect("client");
        let result = client
            .push_outbox(&mut storage, Uuid::new_v4())
            .await
            .expect("push empty outbox");

        assert_eq!(result, PushResult::default());
    }

    // -------------------------------------------------------------
    // Pull Logic Tests (§8 Week 6)
    // -------------------------------------------------------------

    #[tokio::test]
    async fn test_pull_and_apply_dominating_event() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_pull.db");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let remote_device = Uuid::new_v4();
        let entity_id = "rev-pull-001";
        let event_id = Uuid::new_v4();

        let mut clock = VectorClock::new();
        clock.set(remote_device.to_string(), 1);

        let pulled = PulledEvent {
            id: event_id,
            device_id: remote_device,
            entity_type: "revenue".to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 2500, "currency": "USD"}),
            vector_clock: clock.clone(),
            signature: serde_json::Value::String(hex_encode(&vec![7u8; 64])),
            server_seq: Some(10),
            created_at: "2026-11-07T12:00:00Z".to_string(),
        };

        let response_body = serde_json::to_string(&vec![pulled]).expect("serialize");
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .pull_and_apply(&mut storage, None)
            .await
            .expect("pull and apply");

        assert_eq!(result.total_pulled, 1);
        assert_eq!(result.applied_count, 1);
        assert_eq!(result.ignored_count, 0);
        assert_eq!(result.conflict_count, 0);
        assert_eq!(result.last_server_seq, 10);

        // Verify entity_state was created
        let state = storage
            .get_entity_state("revenue", entity_id)
            .expect("query state")
            .expect("state exists");
        assert_eq!(state.state["amount_cents"], 2500);
        assert_eq!(state.vector_clock, clock);
        assert_eq!(storage.get_last_server_seq().unwrap(), 10);
    }

    #[tokio::test]
    async fn test_pull_and_ignore_dominated_event() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_pull_dominated.db");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let device_a = Uuid::new_v4();
        let entity_id = "rev-dominated-001";

        // Seed local state with clock {device_a: 3}
        let mut local_clock = VectorClock::new();
        local_clock.set(device_a.to_string(), 3);

        let local_event = Event {
            id: Uuid::new_v4(),
            entity_type: "revenue".to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 5000}),
            vector_clock: local_clock.clone(),
            signature: vec![1u8; 64],
            synced: 1,
            created_at: "2026-11-07T10:00:00Z".to_string(),
        };
        storage
            .append_event_and_update_state(&local_event, &serde_json::json!({"amount_cents": 5000}))
            .expect("seed local state");

        // Incoming pulled event has older clock {device_a: 1}
        let mut older_clock = VectorClock::new();
        older_clock.set(device_a.to_string(), 1);

        let pulled = PulledEvent {
            id: Uuid::new_v4(),
            device_id: device_a,
            entity_type: "revenue".to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 1000}),
            vector_clock: older_clock,
            signature: serde_json::Value::String(hex_encode(&vec![2u8; 64])),
            server_seq: Some(15),
            created_at: "2026-11-07T09:00:00Z".to_string(),
        };

        let response_body = serde_json::to_string(&vec![pulled]).expect("serialize");
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .pull_and_apply(&mut storage, None)
            .await
            .expect("pull and apply");

        assert_eq!(result.total_pulled, 1);
        assert_eq!(result.applied_count, 0);
        assert_eq!(result.ignored_count, 1);
        assert_eq!(result.conflict_count, 0);

        // Local state MUST NOT be overwritten
        let state = storage
            .get_entity_state("revenue", entity_id)
            .expect("query state")
            .expect("state exists");
        assert_eq!(
            state.state["amount_cents"], 5000,
            "Local state must not be overwritten by dominated event"
        );
        assert_eq!(state.vector_clock, local_clock);
    }

    #[tokio::test]
    async fn test_pull_concurrent_conflict() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("test_pull_conflict.db");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let device_local = Uuid::new_v4();
        let device_remote = Uuid::new_v4();
        let entity_id = "rev-concurrent-001";

        // Local state modified offline: clock {device_local: 1}
        let mut local_clock = VectorClock::new();
        local_clock.set(device_local.to_string(), 1);

        let local_event = Event {
            id: Uuid::new_v4(),
            entity_type: "revenue".to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 7777}),
            vector_clock: local_clock.clone(),
            signature: vec![1u8; 64],
            synced: 0,
            created_at: "2026-11-07T11:00:00Z".to_string(),
        };
        storage
            .append_event_and_update_state(&local_event, &serde_json::json!({"amount_cents": 7777}))
            .expect("seed local state");

        // Incoming pulled event modified concurrently: clock {device_remote: 1}
        let mut remote_clock = VectorClock::new();
        remote_clock.set(device_remote.to_string(), 1);

        let remote_event_id = Uuid::new_v4();
        let pulled = PulledEvent {
            id: remote_event_id,
            device_id: device_remote,
            entity_type: "revenue".to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 8888}),
            vector_clock: remote_clock,
            signature: serde_json::Value::String(hex_encode(&vec![3u8; 64])),
            server_seq: Some(25),
            created_at: "2026-11-07T11:05:00Z".to_string(),
        };

        let response_body = serde_json::to_string(&vec![pulled]).expect("serialize");
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .pull_and_apply(&mut storage, None)
            .await
            .expect("pull and apply");

        assert_eq!(result.total_pulled, 1);
        assert_eq!(result.applied_count, 0);
        assert_eq!(result.ignored_count, 0);
        assert_eq!(result.conflict_count, 1);

        // Strict hard ban: Do not overwrite local changes on concurrent conflict!
        let state = storage
            .get_entity_state("revenue", entity_id)
            .expect("query state")
            .expect("state exists");
        assert_eq!(
            state.state["amount_cents"], 7777,
            "Local state must NOT be overwritten when concurrent conflict is detected"
        );
        assert_eq!(state.vector_clock, local_clock);
    }

    #[tokio::test]
    async fn test_client_get_conflicts() {
        let conflict_id = Uuid::new_v4();
        let event_a = Uuid::new_v4();
        let event_b = Uuid::new_v4();

        let conflicts = vec![ServerConflict {
            id: conflict_id,
            entity_type: "revenue".to_string(),
            entity_id: "rev-conf-01".to_string(),
            event_a_id: event_a,
            event_b_id: event_b,
            status: "unresolved".to_string(),
            created_at: "2026-11-07T12:00:00Z".to_string(),
        }];

        let response_body = serde_json::to_string(&conflicts).expect("serialize conflicts");
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let fetched = client.get_conflicts().await.expect("fetch conflicts");

        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, conflict_id);
        assert_eq!(fetched[0].entity_id, "rev-conf-01");
        assert_eq!(fetched[0].status, "unresolved");
    }

    #[tokio::test]
    async fn test_client_resolve_and_apply() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("resolve_test.db");
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");

        let conflict_id = Uuid::new_v4();
        let winner_id = Uuid::new_v4();
        let entity_id = "rev-resolve-001";
        let entity_type = "revenue";

        // Seed initial local state
        let mut dev1_clock = VectorClock::new();
        dev1_clock.set("dev1".to_string(), 1);
        let initial_event = Event {
            id: Uuid::new_v4(),
            entity_type: entity_type.to_string(),
            entity_id: entity_id.to_string(),
            event_type: "revenue.recorded".to_string(),
            payload: serde_json::json!({"amount_cents": 1000}),
            vector_clock: dev1_clock.clone(),
            signature: vec![1u8; 64],
            synced: 1,
            created_at: "2026-11-07T10:00:00Z".to_string(),
        };
        storage
            .append_event_and_update_state(
                &initial_event,
                &serde_json::json!({"amount_cents": 1000}),
            )
            .expect("seed initial state");

        // Merged clock after resolution: dev1=1, dev2=1
        let mut merged_clock = dev1_clock.clone();
        merged_clock.set("dev2".to_string(), 1);

        let resolution_response = ResolveConflictResponse {
            status: "resolved".to_string(),
            conflict_id,
            winner_event_id: winner_id,
            entity_state: EntityState {
                entity_type: entity_type.to_string(),
                entity_id: entity_id.to_string(),
                state: serde_json::json!({"amount_cents": 2500, "description": "Winning transaction"}),
                vector_clock: merged_clock.clone(),
                updated_at: "2026-11-07T12:00:00Z".to_string(),
            },
        };

        let response_body = serde_json::to_string(&resolution_response).expect("serialize resp");
        let (server_url, _shutdown) = start_mock_server(200, response_body).await;

        let client = SyncClient::new(server_url).expect("client");
        let result = client
            .resolve_and_apply(&mut storage, conflict_id, winner_id)
            .await
            .expect("resolve and apply");

        assert_eq!(result.status, "resolved");
        assert_eq!(result.conflict_id, conflict_id);
        assert_eq!(result.winner_event_id, winner_id);

        // Verify local SQLite entity_state updated to winning state
        let state = storage
            .get_entity_state(entity_type, entity_id)
            .expect("query state")
            .expect("state exists");
        assert_eq!(state.state["amount_cents"], 2500);
        assert_eq!(state.state["description"], "Winning transaction");
        assert_eq!(state.vector_clock.get("dev1"), Some(&1));
        assert_eq!(state.vector_clock.get("dev2"), Some(&1));
    }
}
