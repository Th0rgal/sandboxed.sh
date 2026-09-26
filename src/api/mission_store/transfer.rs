//! A transfer is an action on one mission. The mission row and activation
//! receipt commit in the same SQLite transaction.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Machine {
    Core,
    Node { id: String },
    Client { id: String },
}
impl Machine {
    pub fn label(&self) -> String {
        match self {
            Self::Core => "Core".into(),
            Self::Node { id } => id.clone(),
            Self::Client { .. } => "This computer".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: Uuid,
    pub mission_id: Uuid,
    pub key: String,
    pub revision: u64,
    pub phase: String,
    pub source: Machine,
    pub destination: Machine,
    pub source_revision: String,
    pub source_generation: u64,
    pub generation: u64,
    pub backend: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub source_root: Option<String>,
    pub destination_root: Option<String>,
    pub manifest: Option<crate::machine_transfer::Manifest>,
    pub receipt: Option<serde_json::Value>,
    pub context: String,
    pub created_at: String,
}
impl Transfer {
    pub fn active(&self) -> bool {
        !matches!(self.phase.as_str(), "activated" | "cancelled")
    }
}
