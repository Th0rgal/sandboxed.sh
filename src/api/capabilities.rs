//! `GET /api/capabilities` — what the caller can actually do here.
//!
//! An autonomous agent that wants to know whether it may dispatch a mission or
//! push to GitHub has, until now, had to infer it from whatever fields it could
//! find. On 2026-08-05 one of them read `github_enabled` on `/api/health`,
//! concluded "GitHub is disabled in this environment", and from that concluded
//! it could not dispatch a mission. Neither conclusion follows:
//! `github_enabled` gates the dashboard's *login button* and says nothing about
//! either mission dispatch or git access.
//!
//! The fix is not a better field name — that only makes the next wrong
//! inference less likely, not impossible. It is an endpoint that answers the
//! question directly, so no inference is needed. A capability here reports
//! three things, and the third is the one that ends the guessing:
//!
//! * `available` — can the caller do this, right now;
//! * `detail` — the observed fact behind that verdict, never a restatement of
//!   it ("connected as `octocat`", not "GitHub is available");
//! * `remedy` — what to do when it is unavailable. An agent that is told what
//!   would fix the situation reports a blocker; one that is not invents a
//!   cause.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Serialize;

use super::routes::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    /// Whether the caller can do this now.
    pub available: bool,
    /// The observed fact behind the verdict — what was checked, not a
    /// paraphrase of `available`.
    pub detail: String,
    /// What would make an unavailable capability available. Omitted when the
    /// capability is available, because there is nothing to remedy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl Capability {
    fn available(detail: impl Into<String>) -> Self {
        Self {
            available: true,
            detail: detail.into(),
            remedy: None,
        }
    }

    fn unavailable(detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            available: false,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesResponse {
    /// Keyed by capability name so a client can look one up without knowing
    /// the whole set, and so adding a capability never breaks an old reader.
    pub capabilities: BTreeMap<String, Capability>,
}

/// Report what this deployment can do for the authenticated caller.
///
/// Every verdict comes from live state — the workspace store, the GitHub
/// connection store — not from configuration alone. Configuration says what was
/// intended; these say what is true.
pub async fn get_capabilities(State(state): State<Arc<AppState>>) -> Json<CapabilitiesResponse> {
    let mut capabilities = BTreeMap::new();

    let workspace_count = state.workspaces.list().await.len();
    capabilities.insert(
        "mission_dispatch".to_string(),
        if workspace_count > 0 {
            Capability::available(format!(
                "{workspace_count} workspace(s) configured; POST /api/control/missions"
            ))
        } else {
            Capability::unavailable(
                "no workspaces configured, and a mission needs one to run in",
                "create a workspace: POST /api/workspaces",
            )
        },
    );

    // This is the capability the incident was actually about. The connected
    // account's token is what gets injected into each mission workspace as git
    // credentials (see `workspace::git_credentials`), so a connection here is
    // the difference between a mission that can push and one that cannot.
    capabilities.insert(
        "github_push".to_string(),
        match state.github_connection.get().await {
            Some(connection) => Capability::available(format!(
                "connected as '{}'; the token is injected into mission workspaces as git credentials",
                connection.login
            )),
            None => Capability::unavailable(
                "no GitHub account is connected, so mission workspaces get no git credentials",
                "connect one: POST /api/integrations/github/authorize (device flow, \
                 no per-deployment OAuth App required)",
            ),
        },
    );

    // Included precisely because it is the field that was misread. Naming it
    // for what it gates, next to the two capabilities it does NOT gate, is
    // what makes the distinction hard to miss.
    capabilities.insert(
        "dashboard_github_login".to_string(),
        if state.config.auth.github_enabled() {
            Capability::available("'Sign in with GitHub' is offered on the dashboard login screen")
        } else {
            Capability::unavailable(
                "'Sign in with GitHub' is not offered on the dashboard login screen; \
                 this gates human login only and affects neither mission dispatch nor git access",
                "set GITHUB_OAUTH_CLIENT_ID, GITHUB_OAUTH_CLIENT_SECRET, \
                 GITHUB_OAUTH_ALLOWLIST and JWT_SECRET",
            )
        },
    );

    Json(CapabilitiesResponse { capabilities })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_available_capability_carries_no_remedy() {
        // A remedy on something that already works reads as a required action
        // and sends an agent off to "fix" a healthy deployment.
        let capability = Capability::available("connected as 'octocat'");
        assert!(capability.available);
        assert!(capability.remedy.is_none());
    }

    #[test]
    fn an_unavailable_capability_always_carries_one() {
        let capability = Capability::unavailable("no account connected", "connect one");
        assert!(!capability.available);
        assert_eq!(capability.remedy.as_deref(), Some("connect one"));
    }

    #[test]
    fn the_remedy_is_omitted_from_the_wire_when_absent() {
        let json = serde_json::to_string(&Capability::available("fine")).expect("serialize");
        assert!(!json.contains("remedy"), "got {json}");
    }

    /// The detail must say what was *observed*. A detail that only restates
    /// `available` gives an agent nothing to reason with, which is how the
    /// original misreading happened.
    #[test]
    fn the_detail_names_the_observation_not_the_verdict() {
        let capability = Capability::unavailable(
            "no GitHub account is connected, so mission workspaces get no git credentials",
            "connect one",
        );
        assert!(capability.detail.contains("no GitHub account is connected"));
        assert!(!capability.detail.eq_ignore_ascii_case("unavailable"));
    }

    #[test]
    fn capabilities_are_keyed_by_name() {
        // A map, not a list: a client looks up the one capability it cares
        // about, and adding a new one never shifts an index out from under it.
        let mut capabilities = BTreeMap::new();
        capabilities.insert("github_push".to_string(), Capability::available("ok"));
        let response = CapabilitiesResponse { capabilities };
        let json = serde_json::to_value(&response).expect("serialize");
        assert!(json["capabilities"]["github_push"]["available"]
            .as_bool()
            .unwrap());
    }
}
