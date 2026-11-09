mod clock;
mod crypto;
mod db;
mod models;
mod sync;

use clap::{Parser, Subcommand};
use crypto::Keypair;
use db::Storage;
use models::{Event, RevenuePayload};
use std::path::PathBuf;
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "edgesync")]
#[command(about = "EdgeSync — Offline-First Edge Client", long_about = None)]
struct Cli {
    /// SQLite database file path
    #[arg(short, long, default_value = "edgesync.db")]
    db: PathBuf,

    /// Device key identity file path
    #[arg(short, long, default_value = "device.key")]
    key: PathBuf,

    /// Database encryption key (SQLCipher) (§8 Week 10)
    #[arg(long, env = "EDGESYNC_DB_KEY", global = true)]
    db_key: Option<String>,

    /// Path to root CA certificate for server verification
    #[arg(long, env = "EDGESYNC_CA_CERT", global = true)]
    ca_cert: Option<String>,

    /// Path to client certificate for mTLS authentication
    #[arg(long, env = "EDGESYNC_CLIENT_CERT", global = true)]
    client_cert: Option<String>,

    /// Path to client private key for mTLS authentication
    #[arg(long, env = "EDGESYNC_CLIENT_KEY", global = true)]
    client_key: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

impl Cli {
    pub fn tls_config(&self) -> sync::TlsConfig {
        sync::TlsConfig {
            ca_cert_path: self.ca_cert.clone(),
            client_cert_path: self.client_cert.clone(),
            client_key_path: self.client_key.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize or display device cryptographic identity
    InitDevice,

    /// Add a revenue transaction event in integer minor units (cents)
    AddRevenue {
        /// Amount in minor currency units (cents, e.g. 100 = $1.00)
        #[arg(short, long)]
        amount: i64,

        /// Transaction description
        #[arg(short, long)]
        description: String,

        /// Currency code (default: USD)
        #[arg(short, long, default_value = "USD")]
        currency: String,

        /// Optional entity ID (defaults to new UUID)
        #[arg(long)]
        entity_id: Option<String>,
    },

    /// List pending outbox events awaiting sync
    Outbox,

    /// List all local events in append-only SQLite ledger
    ListEvents,

    /// Display current local materialized state for an entity
    ShowState {
        #[arg(short = 't', long, default_value = "revenue")]
        entity_type: String,

        #[arg(short = 'i', long)]
        entity_id: String,
    },

    /// Compare two vector clocks provided as JSON strings
    CompareClocks {
        /// Clock A JSON representation (e.g. '{"dev1": 1, "dev2": 2}')
        #[arg(long)]
        clock_a: String,

        /// Clock B JSON representation (e.g. '{"dev1": 2, "dev2": 1}')
        #[arg(long)]
        clock_b: String,
    },

    /// Check server health and verify mTLS handshake
    Health {
        /// Base URL of the sync server (e.g. https://localhost:8080)
        #[arg(long, default_value = "https://localhost:8080")]
        server_url: String,
    },

    /// Push pending outbox events to sync server
    Push {
        /// Base URL of the sync server (e.g. http://localhost:8080)
        #[arg(long, default_value = "http://localhost:8080")]
        server_url: String,
    },

    /// Pull new events from sync server and apply to local database
    Pull {
        /// Base URL of the sync server (e.g. http://localhost:8080)
        #[arg(long, default_value = "http://localhost:8080")]
        server_url: String,

        /// Optional server sequence number to pull after (defaults to last tracked seq)
        #[arg(long)]
        after_seq: Option<i64>,
    },

    /// List unresolved conflicts from central server and local database
    Conflicts {
        /// Base URL of the sync server (e.g. http://localhost:8080)
        #[arg(long, default_value = "http://localhost:8080")]
        server_url: String,
    },

    /// Resolve a concurrent conflict by choosing the winning event ID
    Resolve {
        /// Base URL of the sync server (e.g. http://localhost:8080)
        #[arg(long, default_value = "http://localhost:8080")]
        server_url: String,

        /// Conflict ID to resolve
        #[arg(long)]
        conflict_id: Uuid,

        /// Winning event ID
        #[arg(long)]
        winner: Uuid,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let db_path = cli.db.clone();
    let key_path = cli.key.clone();
    let tls_config = cli.tls_config();
    let db_key_opt = cli.db_key.clone();

    let get_db_key = || -> Result<&str, Box<dyn std::error::Error>> {
        match &db_key_opt {
            Some(k) if !k.trim().is_empty() => Ok(k.as_str()),
            _ => Err(
                "Database encryption key required: provide --db-key <key> or set EDGESYNC_DB_KEY"
                    .into(),
            ),
        }
    };

    match cli.command {
        Commands::InitDevice => {
            let keypair = Keypair::load_or_generate(&key_path)?;
            println!("EdgeSync Device Identity Initialized:");
            println!("  Device ID:        {}", keypair.device_id);
            println!("  Public Key (hex): {}", keypair.public_key_hex());
            println!("  Key file path:    {}", key_path.display());
            println!("\nRegister this device on the central server before syncing.");
        }

        Commands::AddRevenue {
            amount,
            description,
            currency,
            entity_id,
        } => {
            let keypair = Keypair::load_or_generate(&key_path)?;
            let mut storage = Storage::open(&db_path, get_db_key()?)?;

            let eid = entity_id.unwrap_or_else(|| Uuid::new_v4().to_string());
            let entity_type = "revenue".to_string();

            // Vector clock starts at 1 for new entity on this device
            let vector_clock = storage.next_vector_clock(&entity_type, &eid, keypair.device_id)?;

            let event_id = Uuid::new_v4();
            let payload = RevenuePayload {
                amount_cents: amount,
                currency: currency.clone(),
                description: Some(description.clone()),
            };
            let payload_value = serde_json::to_value(&payload)?;

            // Canonical message format for Ed25519 signing
            let payload_compact = serde_json::to_string(&payload_value)?;
            let signing_message = format!("{}:{}", event_id, payload_compact);
            let signature = keypair.sign(signing_message.as_bytes());

            let now = iso8601_now();
            let event = Event {
                id: event_id,
                entity_type: entity_type.clone(),
                entity_id: eid.clone(),
                event_type: "revenue.recorded".to_string(),
                payload: payload_value.clone(),
                vector_clock: vector_clock.clone(),
                signature,
                synced: 0,
                created_at: now,
            };

            storage.append_event_and_update_state(&event, &payload_value)?;

            println!("Recorded revenue event successfully:");
            println!("  Event ID:    {}", event_id);
            println!("  Entity ID:   {}", eid);
            println!(
                "  Amount:      ${:.2} ({} cents) {}",
                (amount as f64) / 100.0,
                amount,
                currency
            );
            println!("  Description: {}", description);
            println!("  Clock:       {}", serde_json::to_string(&vector_clock)?);
            println!("  Status:      Enqueued in local outbox (synced: 0)");
        }

        Commands::Outbox => {
            let storage = Storage::open(&db_path, get_db_key()?)?;
            let events = storage.get_outbox_events()?;
            println!("Pending Outbox Events ({}):", events.len());
            for ev in events {
                println!(
                    " - [{}] {} -> {} ({}) [Clock: {}]",
                    ev.id,
                    ev.entity_type,
                    ev.entity_id,
                    ev.event_type,
                    serde_json::to_string(&ev.vector_clock)?
                );
            }
        }

        Commands::ListEvents => {
            let storage = Storage::open(&db_path, get_db_key()?)?;
            let events = storage.get_all_events()?;
            println!("All Local Events ({}):", events.len());
            for ev in events {
                println!(
                    " - [{}] {} -> {} (synced: {}) [Clock: {}]",
                    ev.id,
                    ev.entity_type,
                    ev.entity_id,
                    ev.synced,
                    serde_json::to_string(&ev.vector_clock)?
                );
            }
        }

        Commands::ShowState {
            entity_type,
            entity_id,
        } => {
            let storage = Storage::open(&db_path, get_db_key()?)?;
            match storage.get_entity_state(&entity_type, &entity_id)? {
                Some(state) => {
                    println!("Materialized Entity State:");
                    println!("  Type:    {}", state.entity_type);
                    println!("  ID:      {}", state.entity_id);
                    println!(
                        "  Clock:   {}",
                        serde_json::to_string_pretty(&state.vector_clock)?
                    );
                    println!("  State:   {}", serde_json::to_string_pretty(&state.state)?);
                    println!("  Updated: {}", state.updated_at);
                }
                None => {
                    println!(
                        "No local entity state found for {}/{}",
                        entity_type, entity_id
                    );
                }
            }
        }
        Commands::CompareClocks { clock_a, clock_b } => {
            let a: clock::VectorClock = serde_json::from_str(&clock_a)?;
            let b: clock::VectorClock = serde_json::from_str(&clock_b)?;
            let relation = a.compare(&b);
            let merged = a.merge(&b);
            println!("Clock A:  {}", serde_json::to_string(&a)?);
            println!("Clock B:  {}", serde_json::to_string(&b)?);
            println!("Relation: {}", relation);
            println!("Merged:   {}", serde_json::to_string(&merged)?);
        }
        Commands::Health { server_url } => {
            let sync_client = sync::SyncClient::with_options(
                server_url.clone(),
                std::time::Duration::from_secs(5),
                Some(tls_config),
            )?;
            match sync_client.check_health().await {
                Ok(true) => {
                    println!("Server at {} is healthy (mTLS verified).", server_url);
                }
                Ok(false) => {
                    eprintln!("Server at {} returned non-200 status.", server_url);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Failed to connect to {}: {}", server_url, e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Push { server_url } => {
            let mut storage = Storage::open(&db_path, get_db_key()?)?;
            let keypair = Keypair::load_or_generate(&key_path)?;
            let sync_client = sync::SyncClient::with_options(
                server_url,
                std::time::Duration::from_secs(30),
                Some(tls_config),
            )?;
            let res = sync_client
                .push_outbox(&mut storage, keypair.device_id)
                .await?;

            if res.accepted_count == 0
                && res.ignored_count == 0
                && res.conflict_count == 0
                && res.rejected_count == 0
            {
                println!("Outbox is empty. Nothing to push.");
            } else {
                println!("Sync push summary:");
                println!("  Accepted:   {}", res.accepted_count);
                println!("  Ignored:    {}", res.ignored_count);
                println!("  Conflicts:  {}", res.conflict_count);
                println!("  Rejected:   {}", res.rejected_count);

                for conflict in res.conflicts {
                    println!(
                        "  ⚠️ Conflict: ID {} on {}/{} (concurrent with {})",
                        conflict.id, conflict.entity_type, conflict.entity_id, conflict.event_a_id
                    );
                }
            }
        }
        Commands::Pull {
            server_url,
            after_seq,
        } => {
            let mut storage = Storage::open(&db_path, get_db_key()?)?;
            let sync_client = sync::SyncClient::with_options(
                server_url,
                std::time::Duration::from_secs(30),
                Some(tls_config),
            )?;
            let res = sync_client.pull_and_apply(&mut storage, after_seq).await?;

            if res.total_pulled == 0 {
                println!(
                    "No new events to pull (last known server_seq: {}).",
                    res.last_server_seq
                );
            } else {
                println!("Sync pull summary:");
                println!("  Total Pulled:    {}", res.total_pulled);
                println!("  Applied (newer): {}", res.applied_count);
                println!("  Ignored (older): {}", res.ignored_count);
                println!("  Conflicts:       {}", res.conflict_count);
                println!("  Last Server Seq: {}", res.last_server_seq);
            }
        }
        Commands::Conflicts { server_url } => {
            let storage = Storage::open(&db_path, get_db_key()?)?;
            let local_conflicts = storage.get_unresolved_conflicts()?;

            println!("=== Local Conflicts ({}) ===", local_conflicts.len());
            if local_conflicts.is_empty() {
                println!("  No local unresolved conflicts.");
            } else {
                for c in &local_conflicts {
                    println!("  Conflict ID:     {}", c.id);
                    println!("  Entity:          {}/{}", c.entity_type, c.entity_id);
                    println!("  Remote Event ID: {}", c.remote_event_id);
                    println!(
                        "  Local Clock:     {}",
                        serde_json::to_string(&c.local_clock).unwrap_or_default()
                    );
                    println!(
                        "  Remote Clock:    {}",
                        serde_json::to_string(&c.remote_clock).unwrap_or_default()
                    );
                    println!("  Created At:      {}", c.created_at);
                    println!("  Status:          {}", c.status);
                    println!("  ---");
                }
            }

            println!("\n=== Server Conflicts ===");
            let sync_client = sync::SyncClient::with_options(
                server_url,
                std::time::Duration::from_secs(30),
                Some(tls_config.clone()),
            )?;
            match sync_client.get_conflicts().await {
                Ok(server_conflicts) => {
                    if server_conflicts.is_empty() {
                        println!("  No server unresolved conflicts.");
                    } else {
                        println!(
                            "  Found {} unresolved conflict(s) on server:",
                            server_conflicts.len()
                        );
                        for c in &server_conflicts {
                            println!("  Conflict ID:    {}", c.id);
                            println!("  Entity:         {}/{}", c.entity_type, c.entity_id);
                            println!("  Event A (curr): {}", c.event_a_id);
                            println!("  Event B (new):  {}", c.event_b_id);
                            println!("  Status:         {}", c.status);
                            println!("  Created At:     {}", c.created_at);
                            println!("  ---");
                        }
                    }
                }
                Err(e) => {
                    println!("  ⚠️ Failed to fetch conflicts from server: {}", e);
                }
            }
        }
        Commands::Resolve {
            server_url,
            conflict_id,
            winner,
        } => {
            let mut storage = Storage::open(&db_path, get_db_key()?)?;
            let sync_client = sync::SyncClient::with_options(
                server_url,
                std::time::Duration::from_secs(30),
                Some(tls_config),
            )?;

            println!(
                "Resolving conflict {} with winning event {}...",
                conflict_id, winner
            );
            match sync_client
                .resolve_and_apply(&mut storage, conflict_id, winner)
                .await
            {
                Ok(resp) => {
                    println!("✅ Conflict successfully resolved!");
                    println!("  Status:          {}", resp.status);
                    println!("  Conflict ID:     {}", resp.conflict_id);
                    println!("  Winning Event:   {}", resp.winner_event_id);
                    println!(
                        "  Entity:          {}/{}",
                        resp.entity_state.entity_type, resp.entity_state.entity_id
                    );
                    println!(
                        "  Updated State:   {}",
                        serde_json::to_string_pretty(&resp.entity_state.state).unwrap_or_default()
                    );
                    println!(
                        "  Merged Clock:    {}",
                        serde_json::to_string(&resp.entity_state.vector_clock).unwrap_or_default()
                    );
                    println!("  Updated At:      {}", resp.entity_state.updated_at);
                    println!("Local SQLite entity_state updated to match winning state.");
                }
                Err(e) => {
                    eprintln!("❌ Failed to resolve conflict: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem_secs = (secs % 86400) as u32;

    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;
    let second = rem_secs % 60;

    // Civil calendar algorithm from epoch days (Howard Hinnant algorithm)
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, minute, second
    )
}

fn iso8601_now() -> String {
    let now = SystemTime::now();
    let duration = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format_rfc3339(duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_cli_revenue_workflow() {
        let dir = tempdir().expect("create temp dir");
        let db_path = dir.path().join("test.db");
        let key_path = dir.path().join("test.key");

        // 1. Initialize identity
        let keypair = Keypair::load_or_generate(&key_path).expect("generate keypair");
        assert!(key_path.exists());
        assert_eq!(keypair.public_key_bytes().len(), 32);

        // 2. Open DB and record revenue
        let mut storage = Storage::open(&db_path, "test-encryption-key").expect("open db");
        let entity_id = "rev-test-42";
        let entity_type = "revenue";

        let clock = storage
            .next_vector_clock(entity_type, entity_id, keypair.device_id)
            .expect("next clock");
        assert_eq!(clock.get(&keypair.device_id.to_string()), Some(&1));

        let event_id = Uuid::new_v4();
        let payload = RevenuePayload {
            amount_cents: 9999,
            currency: "USD".to_string(),
            description: Some("Subscription Renewal".to_string()),
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
            vector_clock: clock.clone(),
            signature: sig,
            synced: 0,
            created_at: iso8601_now(),
        };

        storage
            .append_event_and_update_state(&event, &payload_json)
            .expect("save event");

        // 3. Verify in outbox
        let outbox = storage.get_outbox_events().expect("get outbox");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].id, event_id);
        assert_eq!(outbox[0].payload["amount_cents"], 9999);

        // 4. Verify in materialized state
        let state = storage
            .get_entity_state(entity_type, entity_id)
            .expect("get state")
            .expect("state exists");
        assert_eq!(state.state["amount_cents"], 9999);
        assert_eq!(
            state.vector_clock.get(&keypair.device_id.to_string()),
            Some(&1)
        );

        // 5. Add second event for same entity
        let clock2 = storage
            .next_vector_clock(entity_type, entity_id, keypair.device_id)
            .expect("next clock 2");
        assert_eq!(clock2.get(&keypair.device_id.to_string()), Some(&2));
    }

    #[test]
    fn test_format_rfc3339() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        // 2026-11-08T00:00:00Z is 1794096000 seconds
        let dt = format_rfc3339(1794096000);
        assert!(dt.starts_with("2026-11-08"));
    }
}
