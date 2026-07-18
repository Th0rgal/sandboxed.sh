//! Feature gating and capability probing for the Grok tool-bridge route.
//!
//! The route is advertised (and answerable) only when *every* gate passes:
//!   1. `SANDBOXED_SH_GROK_ROUTER_BRIDGE` is on (default off),
//!   2. a **genuine** xAI API key is configured for the grok backend (an
//!      OAuth-only connected account does NOT qualify — the CLI cannot consume
//!      it non-interactively and we never relabel OAuth as an API key),
//!   3. the `grok` CLI binary is actually resolvable on this host (a real,
//!      bounded, cached health check — not just an operator assertion), and
//!   4. `SANDBOXED_SH_GROK_ROUTER_BRIDGE_LIVE` is on — the operator's explicit
//!      assertion that the live ACP↔MCP tool-mediation was verified against
//!      their connected account.
//!
//! Gate 4 exists because caller-owned tool continuation cannot be proven
//! without a provisioned account; until an operator flips it we fail closed and
//! never advertise a route we cannot honor.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::util::env_var_bool;

/// Canonical model id for the connected-account Grok bridge. Distinct from the
/// API-key `xai/grok-4.5` route so the two can never be conflated.
pub const BRIDGE_MODEL_ID: &str = "grok-cli/grok-4.5";

/// The model prefix the proxy intercepts for this bridge.
pub const BRIDGE_MODEL_PREFIX: &str = "grok-cli/";

pub fn feature_enabled() -> bool {
    env_var_bool("SANDBOXED_SH_GROK_ROUTER_BRIDGE", false)
}

/// Whether the live ACP+MCP transport is authorized. Default off: the bridge's
/// pure semantics are proven by tests, but live viability against a real Grok
/// account is an operator-verified deployment gate.
pub fn live_transport_enabled() -> bool {
    env_var_bool("SANDBOXED_SH_GROK_ROUTER_BRIDGE_LIVE", false)
}

/// A **genuine** xAI API key is configured for the grok backend. Non-secret:
/// only presence is checked, never the value. An OAuth-only connected account
/// is deliberately excluded — the bridge cannot use it non-interactively and
/// must never relabel an OAuth token as an API key.
pub fn connected_account_available(working_dir: &Path) -> bool {
    crate::api::ai_providers::get_xai_api_key_for_grok(working_dir).is_some()
}

/// Resolve the `grok` CLI binary: an explicit override path, or a `grok`
/// executable somewhere on `PATH`. Returns `None` when nothing is found.
fn resolve_grok_cli() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("SANDBOXED_SH_GROK_CLI_PATH") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            let path = PathBuf::from(explicit);
            return path.is_file().then_some(path);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("grok"))
        .find(|candidate| candidate.is_file())
}

/// Whether the `grok` CLI is resolvable on this host. A real, bounded health
/// signal (a filesystem probe, no network) cached briefly so repeated catalog
/// probes don't re-stat the filesystem on every request.
pub fn grok_cli_resolvable() -> bool {
    static CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();
    const CACHE_TTL: Duration = Duration::from_secs(30);
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some((at, ok)) = *guard {
        if at.elapsed() < CACHE_TTL {
            return ok;
        }
    }
    let ok = resolve_grok_cli().is_some();
    *guard = Some((Instant::now(), ok));
    ok
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub advertise: bool,
    pub reason: &'static str,
}

/// Decide whether to advertise / answer the bridge route.
pub fn probe(working_dir: &Path) -> Capability {
    if !feature_enabled() {
        return Capability {
            advertise: false,
            reason: "SANDBOXED_SH_GROK_ROUTER_BRIDGE is off",
        };
    }
    if !connected_account_available(working_dir) {
        return Capability {
            advertise: false,
            reason: "no genuine xAI API key configured for the grok backend",
        };
    }
    if !grok_cli_resolvable() {
        return Capability {
            advertise: false,
            reason: "the grok CLI binary is not resolvable on this host",
        };
    }
    if !live_transport_enabled() {
        return Capability {
            advertise: false,
            reason: "SANDBOXED_SH_GROK_ROUTER_BRIDGE_LIVE not enabled (live transport unverified)",
        };
    }
    Capability {
        advertise: true,
        reason: "ready",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_distinct_from_api_key_route() {
        assert_eq!(BRIDGE_MODEL_ID, "grok-cli/grok-4.5");
        assert!(BRIDGE_MODEL_ID.starts_with(BRIDGE_MODEL_PREFIX));
        // Must NOT collide with the xai API-key direct route.
        assert_ne!(BRIDGE_MODEL_ID, "xai/grok-4.5");
    }

    #[test]
    fn probe_fails_closed_when_feature_off() {
        // Default env: feature flag unset ⇒ not advertised regardless of
        // account state.
        let cap = probe(Path::new("/nonexistent"));
        assert!(!cap.advertise);
    }
}
