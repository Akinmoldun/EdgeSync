use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::clock::VectorClock;

/// Event represents an immutable client-generated state transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub vector_clock: VectorClock,
    pub signature: Vec<u8>,
    #[serde(default)]
    pub synced: i64,
    pub created_at: String,
}

/// OutboxEntry tracks pending outgoing events awaiting push confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub event_id: Uuid,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// EntityState records the current consolidated local view of an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    pub entity_type: String,
    pub entity_id: String,
    pub state: serde_json::Value,
    pub vector_clock: VectorClock,
    pub updated_at: String,
}

/// RevenuePayload represents financial transactions using integer minor units (cents).
/// Principle P1: No floats for money.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevenuePayload {
    pub amount_cents: i64,
    pub currency: String,
    pub description: Option<String>,
}
