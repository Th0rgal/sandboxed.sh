//! Reconcile rotated Anthropic OAuth tokens back into the `ai_providers.json`
//! store records — the missing half of the split-brain fix.
//!
//! Background: Anthropic rotates AND revokes the refresh token on every refresh,
//! so every persisted copy of a given account's token must move together. The
//! per-mission Claude Code harness refreshes the token inside its isolated HOME
//! and the runner syncs that back to the shared credential *tiers*
//! (`sync_oauth_to_all_tiers`) — but it never updates the multi-account
//! AIProviderStore *record*. The record then keeps a stale (already-rotated)
//! refresh token, and the proactive store-refresh pass tries to refresh it →
//! `invalid_grant` → Anthropic revokes the whole family → the freshly-rotated
//! tier token dies too. That's the recurring "account revoked" loop.
//!
//! The store is in-memory cached behind a single owner (the AppState
//! `AIProviderStore`), so a second instance can't safely write the file. This
//! module therefore splits the work:
//!   * the runner (which has no store handle) drops a small **pending-rotation
//!     sidecar** recording `old_refresh_token → new_*` (file-only);
//!   * the proactive loop (which owns the store) drains the sidecar and, in the
//!     same cycle, also propagates its own file-tier rotations — matching the
//!     account by its *old* refresh token (unambiguous at rotation time) and
//!     writing the rotated token into that record.
//!
//! Result: the store record always follows the tier, so the proactive pass
//! never refreshes a dead token, and the family is never revoked from under a
//! live mission.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai_providers::{AIProviderStore, OAuthCredentials, ProviderType};

/// One rotation waiting to be reconciled into the store.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PendingRotation {
    /// Provider key, currently only `"anthropic"`.
    pub provider: String,
    /// The refresh token the store record is expected to still hold (the value
    /// before this rotation) — used to locate the owning account.
    pub old_refresh_token: String,
    pub new_refresh_token: String,
    pub new_access_token: String,
    pub expires_at: i64,
}

/// Sidecar path, co-located with `ai_providers.json` under the app working dir
/// (`<working_dir>/.sandboxed-sh/`). It MUST track the store, not `$HOME`:
/// separate dev/prod services on one host share a `$HOME` (e.g.
/// `/var/lib/opencode`) but have distinct working dirs, so a `$HOME`-based
/// sidecar would let the two environments drain and reconcile each other's
/// rotations. Both the runner (`app_working_dir`) and the proactive loop
/// (`config.working_dir`) pass the same working dir, so they resolve the same
/// per-environment file.
pub fn sidecar_path(working_dir: &Path) -> PathBuf {
    working_dir
        .join(".sandboxed-sh")
        .join("anthropic_rotation_pending.json")
}

/// Read the pending queue (empty on missing/corrupt file).
pub fn read_pending(path: &Path) -> Vec<PendingRotation> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Queue a rotation for the loop to reconcile. Idempotent on `new_refresh_token`
/// (re-recording the same rotation replaces the prior entry rather than piling
/// up). Atomic temp+rename write so a crash can't leave a half-written file.
pub fn record_pending_rotation(path: &Path, rot: PendingRotation) -> std::io::Result<()> {
    // A no-op rotation (token unchanged) carries no information.
    if rot.old_refresh_token == rot.new_refresh_token || rot.new_refresh_token.trim().is_empty() {
        return Ok(());
    }
    let mut list = read_pending(path);
    list.retain(|r| r.new_refresh_token != rot.new_refresh_token);
    list.push(rot);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&list)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Remove the sidecar (after a successful drain).
pub fn clear_pending(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Pure matcher: find the account whose current refresh token equals `old` so a
/// rotation can be attributed unambiguously. Kept separate from store I/O so it
/// is trivially unit-testable.
pub fn match_account_by_old_token(accounts: &[(Uuid, Option<String>)], old: &str) -> Option<Uuid> {
    if old.trim().is_empty() {
        return None;
    }
    accounts
        .iter()
        .find(|(_, tok)| tok.as_deref() == Some(old))
        .map(|(id, _)| *id)
}

/// Write a rotated token into the store record of the account that currently
/// holds `old_refresh_token`. Returns the matched account id, or None when no
/// record still holds the old token (already reconciled, or a foreign tier).
pub async fn apply_rotation(
    store: &AIProviderStore,
    provider_type: ProviderType,
    rot: &PendingRotation,
) -> Option<Uuid> {
    let accounts: Vec<(Uuid, Option<String>)> = store
        .get_all_by_type(provider_type)
        .await
        .into_iter()
        .map(|a| (a.id, a.oauth.map(|o| o.refresh_token)))
        .collect();

    let id = match_account_by_old_token(&accounts, &rot.old_refresh_token)?;
    store
        .set_oauth_credentials(
            id,
            OAuthCredentials {
                access_token: rot.new_access_token.clone(),
                refresh_token: rot.new_refresh_token.clone(),
                expires_at: rot.expires_at,
            },
        )
        .await;
    // The record now carries a live token again — lift any cached invalid_grant.
    crate::api::ai_providers::oauth_refresh_clear_dead(id);
    tracing::info!(
        account_id = %id,
        provider = ?provider_type,
        new_expires_at = rot.expires_at,
        "Reconciled store record from out-of-band tier rotation"
    );
    Some(id)
}

/// Drain every queued rotation into the store, then clear the sidecar. Called at
/// the top of each proactive-refresh cycle, BEFORE the store/tier refresh passes
/// (so a stale record is healed before anything tries to refresh it).
pub async fn drain_pending(store: &AIProviderStore, path: &Path) -> u32 {
    let list = read_pending(path);
    if list.is_empty() {
        return 0;
    }
    let mut applied = 0u32;
    for rot in &list {
        let provider_type = match rot.provider.as_str() {
            "anthropic" => ProviderType::Anthropic,
            _ => continue,
        };
        if apply_rotation(store, provider_type, rot).await.is_some() {
            applied += 1;
        }
    }
    clear_pending(path);
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_by_old_token_finds_owner() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let accounts = vec![
            (a, Some("tok-A".to_string())),
            (b, Some("tok-B".to_string())),
        ];
        assert_eq!(match_account_by_old_token(&accounts, "tok-B"), Some(b));
        assert_eq!(match_account_by_old_token(&accounts, "tok-A"), Some(a));
        assert_eq!(match_account_by_old_token(&accounts, "tok-X"), None);
        assert_eq!(match_account_by_old_token(&accounts, ""), None);
    }

    #[test]
    fn match_ignores_accounts_without_token() {
        let a = Uuid::new_v4();
        let accounts = vec![(a, None), (Uuid::new_v4(), Some("other".to_string()))];
        assert_eq!(match_account_by_old_token(&accounts, "missing"), None);
        let _ = a;
    }

    #[test]
    fn sidecar_roundtrip_dedup_and_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("pending.json");
        let mk = |old: &str, new: &str| PendingRotation {
            provider: "anthropic".into(),
            old_refresh_token: old.into(),
            new_refresh_token: new.into(),
            new_access_token: "acc".into(),
            expires_at: 123,
        };
        // no-op (old==new) is dropped
        record_pending_rotation(&path, mk("same", "same")).unwrap();
        assert!(read_pending(&path).is_empty());

        record_pending_rotation(&path, mk("old1", "new1")).unwrap();
        record_pending_rotation(&path, mk("old2", "new2")).unwrap();
        assert_eq!(read_pending(&path).len(), 2);

        // re-recording the same new token replaces, not appends
        record_pending_rotation(&path, mk("old1b", "new1")).unwrap();
        let list = read_pending(&path);
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.iter()
                .find(|r| r.new_refresh_token == "new1")
                .unwrap()
                .old_refresh_token,
            "old1b"
        );

        clear_pending(&path);
        assert!(read_pending(&path).is_empty());
    }

    #[tokio::test]
    async fn drain_updates_matching_store_record_and_skips_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store = AIProviderStore::new(dir.path().join("ai_providers.json")).await;
        let mut p = crate::ai_providers::AIProvider::new(ProviderType::Anthropic, "Acct".into());
        p.oauth = Some(OAuthCredentials {
            access_token: "old-acc".into(),
            refresh_token: "OLD".into(),
            expires_at: 1,
        });
        let id = store.add(p).await;

        let path = dir.path().join("pending.json");
        record_pending_rotation(
            &path,
            PendingRotation {
                provider: "anthropic".into(),
                old_refresh_token: "OLD".into(),
                new_refresh_token: "NEW".into(),
                new_access_token: "new-acc".into(),
                expires_at: 999,
            },
        )
        .unwrap();

        assert_eq!(drain_pending(&store, &path).await, 1);
        let o = store.get(id).await.unwrap().oauth.unwrap();
        assert_eq!(o.refresh_token, "NEW");
        assert_eq!(o.access_token, "new-acc");
        assert_eq!(o.expires_at, 999);
        assert!(read_pending(&path).is_empty()); // sidecar cleared

        // A rotation whose old token no longer matches any record is a no-op
        // (e.g. it belonged to a different account / was already reconciled).
        record_pending_rotation(
            &path,
            PendingRotation {
                provider: "anthropic".into(),
                old_refresh_token: "OLD".into(),
                new_refresh_token: "NEWER".into(),
                new_access_token: "x".into(),
                expires_at: 1000,
            },
        )
        .unwrap();
        assert_eq!(drain_pending(&store, &path).await, 0);
        assert_eq!(
            store.get(id).await.unwrap().oauth.unwrap().refresh_token,
            "NEW"
        );
    }
}
