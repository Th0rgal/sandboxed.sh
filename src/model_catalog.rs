//! Connection-scoped discovery and portable, reviewed fallback snapshots.
//! Runtime observations never contain credentials or endpoint URLs.
use crate::api::providers::ProviderModel;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub provider_id: String,
    pub access_profile: String,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub completeness: Completeness,
    pub models: Vec<ProviderModel>,
}

fn safe_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported snapshot schema".into());
        }
        if !safe_slug(&self.provider_id) || !safe_slug(&self.access_profile) {
            return Err("invalid provider/profile".into());
        }
        if !["curated", "discovery"].contains(&self.source.as_str()) {
            return Err("invalid snapshot source".into());
        }
        if self.source == "discovery" && self.observed_at.is_none() {
            return Err("discovery snapshot needs observed_at".into());
        }
        if self.models.is_empty() {
            return Err("empty snapshot cannot replace fallback models".into());
        }
        let mut ids = BTreeSet::new();
        for model in &self.models {
            if model.id.trim() != model.id
                || model.id.is_empty()
                || model.id.len() > 256
                || model.id.chars().any(char::is_control)
                || model.id.contains("://")
                || model.id.contains('@')
                || !ids.insert(&model.id)
            {
                return Err(format!("invalid or duplicate model ID: {}", model.id));
            }
            if self.provider_id == "zai" && model.id == "glm-5.3[1m]" {
                return Err("use glm-5.3; context size is metadata, not an API ID".into());
            }
        }
        Ok(())
    }
    pub fn filename(&self) -> String {
        format!("{}-{}.json", self.provider_id, self.access_profile)
    }
}

pub fn bundled_snapshots() -> Vec<Snapshot> {
    BUNDLED
        .iter()
        .map(|raw| {
            let s: Snapshot = serde_json::from_str(raw).expect("bundled snapshot JSON");
            s.validate().expect("valid bundled snapshot");
            s
        })
        .collect()
}
pub fn default_profile(provider: &str) -> &str {
    match provider {
        "zai" | "kimi" => "coding",
        "google" => "oauth",
        _ => "api",
    }
}
pub fn bundled_models(provider: &str, profile: &str) -> Vec<ProviderModel> {
    bundled_snapshots()
        .into_iter()
        .find(|s| s.provider_id == provider && s.access_profile == profile)
        .map(|s| s.models)
        .unwrap_or_default()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Success {
    pub observed_at: DateTime<Utc>,
    pub completeness: Completeness,
    pub models: Vec<ProviderModel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    /// Hash of account UUID, route and auth profile; no email or endpoint.
    pub connection: String,
    pub provider_id: String,
    pub access_profile: String,
    pub exportable: bool,
    pub checked_at: DateTime<Utc>,
    /// discovered, error, or unsupported. Errors deliberately omit upstream text.
    pub status: String,
    pub diagnostic: Option<String>,
    pub last_success: Option<Success>,
    #[serde(default)]
    pub fallback_models: Vec<ProviderModel>,
}
impl Observation {
    pub fn effective_models(&self) -> Vec<ProviderModel> {
        let mut models = BTreeMap::new();
        if self
            .last_success
            .as_ref()
            .is_none_or(|s| s.completeness == Completeness::Partial)
        {
            for m in &self.fallback_models {
                models.insert(m.id.clone(), m.clone());
            }
        }
        if let Some(success) = &self.last_success {
            for m in &success.models {
                models.insert(m.id.clone(), m.clone());
            }
        }
        models.into_values().collect()
    }
    pub fn effective_source(&self) -> &'static str {
        match (&self.last_success, self.status.as_str()) {
            (Some(_), "discovered") => "discovery",
            (Some(_), _) => "stale_discovery",
            _ => "snapshot",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscoveryState {
    pub schema_version: u32,
    pub connections: Vec<Observation>,
}
pub fn route_key(account: &str, endpoint: &str, profile: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{account}\0{endpoint}\0{profile}").as_bytes())
    )
}
pub fn state_path(root: &Path) -> std::path::PathBuf {
    root.join(".sandboxed-sh/model-discovery.json")
}
pub fn read_state(root: &Path) -> DiscoveryState {
    std::fs::read(state_path(root))
        .ok()
        .and_then(|b| serde_json::from_slice::<DiscoveryState>(&b).ok())
        .filter(|s| s.schema_version == 1)
        .unwrap_or(DiscoveryState {
            schema_version: 1,
            connections: vec![],
        })
}
pub fn write_state(root: &Path, state: &DiscoveryState) -> Result<(), String> {
    let path = state_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())
}

/// Export only successful public-route observations. Never serialize account IDs,
/// endpoint URLs, errors, custom models or provider-returned free-form text.
pub fn export_snapshots(state: &DiscoveryState) -> Vec<Snapshot> {
    let mut groups: BTreeMap<(String, String), Snapshot> = BTreeMap::new();
    for o in &state.connections {
        if !o.exportable || o.status != "discovered" {
            continue;
        }
        let Some(success) = &o.last_success else {
            continue;
        };
        if success.models.is_empty() {
            continue;
        }
        let key = (o.provider_id.clone(), o.access_profile.clone());
        let s = groups.entry(key).or_insert_with(|| Snapshot {
            schema_version: 1,
            provider_id: o.provider_id.clone(),
            access_profile: o.access_profile.clone(),
            source: "discovery".into(),
            observed_at: Some(success.observed_at),
            // A union across accounts is an observation, not universal entitlement.
            completeness: success.completeness.clone(),
            models: vec![],
        });
        if success.completeness == Completeness::Partial {
            s.completeness = Completeness::Partial;
        }
        s.observed_at = s.observed_at.map(|d| d.max(success.observed_at));
        for m in &success.models {
            s.models.push(ProviderModel {
                id: m.id.clone(),
                name: m.id.clone(),
                description: None,
            });
        }
    }
    groups
        .into_values()
        .filter_map(|mut s| {
            // A failed sibling account must not turn a partial account view
            // into an authoritative deletion from the shared profile snapshot.
            if state.connections.iter().any(|o| {
                o.exportable
                    && o.provider_id == s.provider_id
                    && o.access_profile == s.access_profile
                    && o.status != "discovered"
            }) {
                s.completeness = Completeness::Partial;
            }
            s.models.sort_by(|a, b| a.id.cmp(&b.id));
            s.models.dedup_by(|a, b| a.id == b.id);
            s.validate().ok().map(|_| s)
        })
        .collect()
}

pub fn merge_snapshot(existing: &Snapshot, incoming: &Snapshot) -> Result<Snapshot, String> {
    existing.validate()?;
    incoming.validate()?;
    if existing.provider_id != incoming.provider_id
        || existing.access_profile != incoming.access_profile
    {
        return Err("snapshot profile mismatch".into());
    }
    let mut result = incoming.clone();
    if incoming.completeness == Completeness::Partial {
        let mut all: BTreeMap<String, ProviderModel> = existing
            .models
            .iter()
            .map(|m| (m.id.clone(), m.clone()))
            .collect();
        for m in &incoming.models {
            all.insert(m.id.clone(), m.clone());
        }
        result.models = all.into_values().collect();
    }
    result.validate()?;
    Ok(result)
}

pub fn snapshot_diff(old: &Snapshot, new: &Snapshot) -> serde_json::Value {
    let before: BTreeMap<_, _> = old.models.iter().map(|m| (&m.id, m)).collect();
    let after: BTreeMap<_, _> = new.models.iter().map(|m| (&m.id, m)).collect();
    serde_json::json!({
        "provider_id": new.provider_id, "access_profile":new.access_profile,
        "completeness":new.completeness,
        "added":after.keys().filter(|id| !before.contains_key(*id)).collect::<Vec<_>>(),
        "absent":before.keys().filter(|id| !after.contains_key(*id)).collect::<Vec<_>>(),
        "removals_authoritative":new.completeness == Completeness::Complete,
        "changed":after.iter().filter_map(|(id,m)| before.get(id).filter(|old| old.name != m.name || old.description != m.description).map(|_| id)).collect::<Vec<_>>()
    })
}

include!(concat!(env!("OUT_DIR"), "/model_snapshots.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    fn model(id: &str) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            name: id.into(),
            description: None,
        }
    }
    fn observation() -> Observation {
        Observation {
            connection: "opaque".into(),
            provider_id: "zai".into(),
            access_profile: "coding".into(),
            exportable: true,
            checked_at: Utc::now(),
            status: "discovered".into(),
            diagnostic: None,
            last_success: Some(Success {
                observed_at: Utc::now(),
                completeness: Completeness::Complete,
                models: vec![model("new")],
            }),
            fallback_models: vec![model("old")],
        }
    }
    #[test]
    fn bundled_snapshots_validate_and_profiles_do_not_collide() {
        let snapshots = bundled_snapshots();
        let mut names = BTreeSet::new();
        for snapshot in snapshots {
            snapshot.validate().unwrap();
            assert!(names.insert(snapshot.filename()));
        }
        assert!(bundled_models("zai", "coding")
            .iter()
            .any(|m| m.id == "glm-5.3"));
    }
    #[test]
    fn complete_replaces_partial_supplements() {
        let mut o = observation();
        assert_eq!(
            o.effective_models()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new"]
        );
        o.last_success.as_mut().unwrap().completeness = Completeness::Partial;
        assert_eq!(o.effective_models().len(), 2);
    }
    #[test]
    fn failures_and_empty_responses_keep_previous_success() {
        let mut o = observation();
        crate::model_discovery::record_result(&mut o, Err("http_429".into()));
        assert_eq!(o.effective_source(), "stale_discovery");
        assert_eq!(o.effective_models()[0].id, "new");
        crate::model_discovery::record_result(&mut o, Ok((vec![], Completeness::Complete)));
        assert_eq!(o.effective_models()[0].id, "new");
        assert_eq!(o.diagnostic.as_deref(), Some("empty_catalog"));
    }
    #[test]
    fn account_route_and_auth_profile_are_isolated() {
        assert_ne!(
            route_key("a", "https://a/v1", "api"),
            route_key("b", "https://a/v1", "api")
        );
        assert_ne!(
            route_key("a", "https://a/v1", "api"),
            route_key("a", "https://b/v1", "api")
        );
        assert_ne!(
            route_key("a", "https://a/v1", "api"),
            route_key("a", "https://a/v1", "oauth")
        );
    }
    #[test]
    fn export_excludes_private_routes_stale_errors_and_account_metadata() {
        let mut o = observation();
        o.connection = "SECRET_ACCOUNT".into();
        o.last_success.as_mut().unwrap().models[0].name = "PRIVATE_EMAIL".into();
        let mut private = o.clone();
        private.exportable = false;
        let mut stale = o.clone();
        stale.status = "error".into();
        let out = export_snapshots(&DiscoveryState {
            schema_version: 1,
            connections: vec![o, private, stale],
        });
        assert_eq!(out.len(), 1);
        let raw = serde_json::to_string(&out).unwrap();
        assert!(!raw.contains("SECRET_ACCOUNT"));
        assert!(!raw.contains("PRIVATE_EMAIL"));
        assert_eq!(out[0].models.len(), 1);
        // A failed sibling account makes absence non-authoritative.
        assert_eq!(out[0].completeness, Completeness::Partial);
    }
    #[test]
    fn partial_updates_cannot_remove_existing_models() {
        let old = bundled_snapshots().remove(0);
        let mut incoming = old.clone();
        incoming.models = vec![model("new-model")];
        let merged = merge_snapshot(&old, &incoming).unwrap();
        assert_eq!(merged.models.len(), old.models.len() + 1);
        incoming.completeness = Completeness::Complete;
        assert_eq!(merge_snapshot(&old, &incoming).unwrap().models.len(), 1);
        incoming.models.clear();
        assert!(merge_snapshot(&old, &incoming).is_err());
    }
    #[test]
    fn persistence_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let state = DiscoveryState {
            schema_version: 1,
            connections: vec![observation()],
        };
        write_state(dir.path(), &state).unwrap();
        assert_eq!(
            read_state(dir.path()).connections[0].effective_models()[0].id,
            "new"
        );
    }
}
