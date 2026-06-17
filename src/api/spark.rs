//! DGX Spark build-offload endpoint.
//!
//! A harness inside a mission workspace calls `POST /api/spark/offload` over the
//! host veth link (`10.88.0.1`) to run a Lean build on the DGX Spark instead of
//! the main box. The HOST holds the Spark credentials (arbiter token + SSH
//! target, from [`crate::config::Config`]), so workspaces never carry them.
//!
//! Flow: rsync the mission workspace to the Spark, submit the build to the
//! arbiter (`dgx-spark-arbiter`, which time-shares the Spark's unified memory
//! against vLLM/step37 by priority), poll to completion, rsync artifacts back,
//! return the result. Responds `503` when Spark config is absent so the
//! in-workspace `spark-build` wrapper transparently falls back to a local build.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::mission_store::MissionStore;
use super::routes::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/offload", post(offload_build))
}

/// Secret used to sign the per-mission spark-offload capability token. Reuses
/// the same internal-action secret as Telegram action tokens.
fn spark_offload_secret() -> Option<String> {
    std::env::var("SANDBOXED_INTERNAL_ACTION_SECRET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("JWT_SECRET")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
}

/// Mint a per-mission, scope-bound capability token for spark offload. Unlike
/// the master `SANDBOXED_PROXY_SECRET`, a leaked token only authorizes spark
/// builds for THIS mission — it can't be replayed against the LLM proxy or any
/// other proxy route, nor against another mission's workspace. Returns `None`
/// when no signing secret is configured.
pub fn build_spark_offload_token(mission_id: Uuid) -> Option<String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let secret = spark_offload_secret()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(b"spark-offload:");
    mac.update(mission_id.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

fn verify_spark_offload_token(mission_id: Uuid, token: &str) -> bool {
    let Some(expected) = build_spark_offload_token(mission_id) else {
        return false;
    };
    super::auth::constant_time_eq(&expected, token.trim())
}

/// Extract the bearer token from the `Authorization` header.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Deserialize)]
struct OffloadRequest {
    /// Mission this build belongs to. The bearer token must be the scoped
    /// capability token minted for exactly this mission, and `host_dir` must
    /// resolve to this mission's workspace directory.
    mission_id: Uuid,
    /// Host filesystem path of the mission workspace root, injected as
    /// `SPARK_WORKSPACE_HOST_DIR` and echoed back by the wrapper.
    host_dir: String,
    /// Build cwd relative to the workspace root (e.g. `"morpho-verity"`).
    #[serde(default)]
    rel: String,
    /// The build command, e.g. `"lake build"`.
    cmd: String,
    #[serde(default = "default_priority")]
    priority: String,
}

fn default_priority() -> String {
    "P0".to_string()
}

#[derive(Serialize)]
struct OffloadResponse {
    exit_code: i64,
    log: String,
}

/// Validate and canonicalize a caller-supplied mission workspace path.
///
/// Returns the resolved absolute path only when it is a real directory shaped
/// like `<root>/workspaces/mission-*` (the layout produced by
/// `workspace::mission_workspace_dir_for_root`, for both container and host
/// workspaces). `canonicalize` resolves symlinks and requires existence, so a
/// symlink whose target escapes that layout — or any path merely *containing*
/// the `/workspaces/mission-` substring — is rejected.
fn canonical_mission_host_dir(raw: &str) -> Result<String, &'static str> {
    // Cheap pre-filter before touching the filesystem.
    if !raw.starts_with('/') || raw.contains("..") {
        return Err("invalid host_dir");
    }
    let canon = std::fs::canonicalize(raw).map_err(|_| "invalid host_dir")?;
    if !canon.is_dir() {
        return Err("invalid host_dir");
    }
    let is_mission = canon
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("mission-"));
    let in_workspaces = canon
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "workspaces");
    if !is_mission || !in_workspaces {
        return Err("invalid host_dir");
    }
    canon
        .to_str()
        .map(|s| s.to_string())
        .ok_or("invalid host_dir")
}

/// Run a host subprocess, returning (success, combined output).
async fn run(args: &[&str]) -> (bool, String) {
    match tokio::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .await
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), s)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// Whether a slash-trimmed `rel` is safe to interpolate into the remote build
/// path. The path is re-parsed by the Spark's login shell (via `ssh`/rsync), so
/// only `[A-Za-z0-9._-]` components are allowed — no shell metacharacters, no
/// `..` traversal, no argv-flag-leading (`-`) component. Empty = workspace root.
fn rel_path_is_safe(rel_clean: &str) -> bool {
    rel_clean.is_empty()
        || rel_clean.split('/').all(|c| {
            !c.is_empty()
                && c != ".."
                && !c.starts_with('-')
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
}

/// Walk a mission's `parent_mission_id` chain and return true if any ANCESTOR's
/// id begins with `short` — the 8-char prefix encoded in a `mission-<short>`
/// workspace dir name. Lets an orchestrator worker offload a build that lives in
/// its parent/boss mission's shared workspace dir (per-PR worktrees). The walk
/// is bounded; a missing record or cyclic chain just yields `false` (→ 403).
async fn mission_has_ancestor_short(
    store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    short: &str,
) -> bool {
    if short.is_empty() {
        return false;
    }
    let mut cur = match store.get_mission(mission_id).await {
        Ok(Some(m)) => m.parent_mission_id,
        _ => None,
    };
    let mut depth = 0;
    while let Some(id) = cur {
        let id_str = id.to_string();
        if id_str.len() >= 8 && id_str[..8] == *short {
            return true;
        }
        cur = match store.get_mission(id).await {
            Ok(Some(m)) => m.parent_mission_id,
            _ => None,
        };
        depth += 1;
        if depth > 64 {
            break;
        }
    }
    false
}

async fn offload_build(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OffloadRequest>,
) -> axum::response::Response {
    // Host-privileged endpoint (rsyncs host paths, SSHes to the Spark). Auth is
    // a per-mission, scope-bound capability token — NOT the master proxy secret
    // — so a token leaked from a workspace can't be replayed against the LLM
    // proxy or another mission. The legitimate caller is the in-workspace
    // `spark-build` wrapper, handed the token minted for its own mission.
    let Some(token_in) = bearer_token(&headers) else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    if !verify_spark_offload_token(req.mission_id, &token_in) {
        return (StatusCode::UNAUTHORIZED, "invalid spark offload token").into_response();
    }

    // Security: confine host_dir to a genuine mission workspace directory.
    // A substring check (`contains("/workspaces/mission-")`) is not real
    // containment — an attacker-controlled path or a symlink like
    // `<workspace>/workspaces/mission-x -> /etc` would pass it, and the
    // download rsync below WRITES into host_dir (arbitrary host write as root).
    // So canonicalize (resolving symlinks, requiring existence) and assert the
    // resolved path is `<root>/workspaces/mission-*` — the layout every
    // container and host workspace uses. Using the canonical path for the rsync
    // also removes the argv flag-smuggling surface (it can never start with `-`).
    let host_dir = match canonical_mission_host_dir(&req.host_dir) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    if req.cmd.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "cmd required").into_response();
    }

    let name = host_dir.rsplit('/').next().unwrap_or("build").to_string();
    // The token authenticates a specific mission; the workspace dir is named
    // `mission-<short>`. A mission's scoped token may target its OWN workspace
    // dir, OR the shared workspace dir of an ANCESTOR mission. Orchestrator
    // fleets build in per-PR git worktrees under the boss mission's dir
    // (`mission-<boss>/wk-NNNN`), so a worker legitimately offloads a build that
    // physically lives in its parent's dir. The worker already has filesystem
    // r/w to that shared dir (its harness operates there), so authorizing the
    // offload into it is no privilege escalation — but an UNRELATED mission's
    // dir (not in the caller's ancestor chain) is still rejected, since the
    // rsync below WRITES into host_dir.
    let host_short = name.strip_prefix("mission-").unwrap_or("");
    let req_short = &req.mission_id.to_string()[..8];
    let host_dir_authorized = if host_short.is_empty() {
        false
    } else if host_short == req_short {
        true
    } else {
        let store = state.control.get_mission_store().await;
        mission_has_ancestor_short(&store, req.mission_id, host_short).await
    };
    if !host_dir_authorized {
        return (
            StatusCode::FORBIDDEN,
            "host_dir does not belong to this mission or an ancestor",
        )
            .into_response();
    }

    // Availability check runs AFTER authorization so an unauthorized caller can
    // never probe whether Spark is configured. All three must be set, else tell
    // the (authorized) caller to build locally via a 503.
    let (Some(url), Some(token), Some(ssh)) = (
        state.config.spark_arbiter_url.as_deref(),
        state.config.spark_arbiter_token.as_deref(),
        state.config.spark_ssh_target.as_deref(),
    ) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "spark offload not configured",
        )
            .into_response();
    };
    let user = ssh.split('@').next().unwrap_or("th0rgal");
    let remote_rel = format!(".spark-builds/{}", name);

    // Confine the transfer to the build subtree (`rel`) rather than the whole
    // mission workspace. A mega-mission can carry dozens of multi-GB Lean
    // worktrees (84GB+), and rsyncing the entire root per build is
    // impractical; the build only needs its own self-contained package dir.
    // Reject path traversal so `rel` can't escape the (already mission-bound)
    // host_dir.
    let rel_clean = req.rel.trim_matches('/');
    // `rel` is interpolated into `remote_cwd`, which is re-parsed by the remote
    // login shell (both `ssh … mkdir -p <remote_cwd>` and rsync's `host:path`
    // spec). A scoped-token holder must NOT be able to smuggle shell
    // metacharacters or argv flags onto the Spark (where the ssh user has
    // sudo). Strict allowlist: each path component is non-empty, not `..`, not
    // a flag (`-`-leading), and only `[A-Za-z0-9._-]`. Empty rel = build at the
    // workspace root, which is allowed.
    if !rel_path_is_safe(rel_clean) {
        return (StatusCode::BAD_REQUEST, "invalid rel").into_response();
    }
    let remote_cwd = if rel_clean.is_empty() {
        format!("/home/{}/{}", user, remote_rel)
    } else {
        format!("/home/{}/{}/{}", user, remote_rel, rel_clean)
    };
    // Local source + remote destination, both scoped to the build subtree.
    let local_src = if rel_clean.is_empty() {
        format!("{}/", host_dir)
    } else {
        format!("{}/{}/", host_dir, rel_clean)
    };
    let remote_dst = format!("{}:{}/", ssh, remote_cwd);

    // rsync won't create missing parent dirs; ensure the (possibly nested)
    // remote build dir exists first.
    let mk = run(&["ssh", "--", ssh, "mkdir", "-p", &remote_cwd]).await;
    if !mk.0 {
        tracing::warn!("spark offload: remote mkdir failed: {}", mk.1);
        return (
            StatusCode::BAD_GATEWAY,
            format!("remote mkdir failed: {}", mk.1),
        )
            .into_response();
    }

    // 1. Sync the build subtree up to the Spark.
    let up = run(&[
        "rsync",
        "-az",
        "--delete",
        "--exclude",
        ".git",
        "-e",
        "ssh",
        "--",
        &local_src,
        &remote_dst,
    ])
    .await;
    if !up.0 {
        tracing::warn!("spark offload: rsync up failed: {}", up.1);
        return (
            StatusCode::BAD_GATEWAY,
            format!("rsync up failed: {}", up.1),
        )
            .into_response();
    }

    // 2. Submit the build to the arbiter.
    let client = &state.http_client;
    let submit = client
        .post(format!("{}/build", url))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "priority": req.priority, "cmd": req.cmd, "cwd": remote_cwd,
        }))
        .send()
        .await;
    let jid = match submit {
        Ok(r) => match r.json::<serde_json::Value>().await {
            Ok(v) => v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()),
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("arbiter submit parse: {e}"),
                )
                    .into_response()
            }
        },
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("arbiter unreachable: {e}")).into_response()
        }
    };
    let Some(jid) = jid else {
        return (StatusCode::BAD_GATEWAY, "arbiter returned no job id").into_response();
    };

    // 3. Poll to completion (cap ~60 min — Lean builds can be long).
    let mut log = String::new();
    let mut exit_code = -1i64;
    let mut done = false;
    for _ in 0..1200 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let st = client
            .get(format!("{}/build/{}", url, jid))
            .bearer_auth(token)
            .send()
            .await
            .ok();
        let Some(v) = st else { continue };
        let Ok(v) = v.json::<serde_json::Value>().await else {
            continue;
        };
        log = v
            .get("log_tail")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        match v.get("status").and_then(|x| x.as_str()) {
            Some("done") | Some("failed") => {
                exit_code = v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1);
                done = true;
                break;
            }
            _ => continue,
        }
    }
    if !done {
        return (StatusCode::GATEWAY_TIMEOUT, "build timed out on spark").into_response();
    }

    // 4. Sync artifacts (.olean etc.) back into the build subtree.
    let _back = run(&[
        "rsync",
        "-az",
        "-e",
        "ssh",
        "--",
        &format!("{}:{}/", ssh, remote_cwd),
        &local_src,
    ])
    .await;

    Json(OffloadResponse { exit_code, log }).into_response()
}

#[cfg(test)]
mod tests {
    use super::mission_has_ancestor_short;
    use super::rel_path_is_safe;
    use crate::api::mission_store::{InMemoryMissionStore, MissionStore};
    use std::sync::Arc;

    async fn mk(
        store: &Arc<dyn MissionStore>,
        title: &str,
        parent: Option<uuid::Uuid>,
    ) -> uuid::Uuid {
        store
            .create_mission_with_parent(
                Some(title),
                None,
                None,
                None,
                None,
                None,
                None,
                parent,
                None,
            )
            .await
            .expect("create mission")
            .id
    }

    fn short(id: uuid::Uuid) -> String {
        id.to_string()[..8].to_string()
    }

    #[tokio::test]
    async fn ancestry_matches_parent_and_grandparent_not_strangers() {
        let store: Arc<dyn MissionStore> = Arc::new(InMemoryMissionStore::new());
        let boss = mk(&store, "boss", None).await;
        let worker = mk(&store, "worker", Some(boss)).await;
        let grandchild = mk(&store, "grandchild", Some(worker)).await;
        let stranger = mk(&store, "stranger", None).await;

        // A worker's token may offload into its boss (parent) dir.
        assert!(mission_has_ancestor_short(&store, worker, &short(boss)).await);
        // …and a grandchild reaches the boss two hops up.
        assert!(mission_has_ancestor_short(&store, grandchild, &short(boss)).await);
        assert!(mission_has_ancestor_short(&store, grandchild, &short(worker)).await);
        // An unrelated mission's dir is never authorized.
        assert!(!mission_has_ancestor_short(&store, worker, &short(stranger)).await);
        // The own-dir case is handled by the caller (host_short == req_short),
        // so the ancestor walk (parents only) must NOT self-match.
        assert!(!mission_has_ancestor_short(&store, worker, &short(worker)).await);
        // Empty short never authorizes.
        assert!(!mission_has_ancestor_short(&store, worker, "").await);
        // Unknown mission id → no ancestors → false.
        assert!(!mission_has_ancestor_short(&store, uuid::Uuid::new_v4(), &short(boss)).await);
    }

    #[test]
    fn rel_allows_real_verity_worktrees() {
        for ok in [
            "",
            "verity",
            "morpho-verity",
            "wt-c13-0x20-bridge/verity",
            "wt-catalog-claude/verity",
            "wk-2009",
            "w-2005",
            "a.b/c_d-e",
        ] {
            assert!(rel_path_is_safe(ok), "should allow {ok:?}");
        }
    }

    #[test]
    fn rel_rejects_injection_and_traversal() {
        for bad in [
            "verity; rm -rf ~",
            "verity && reboot",
            "$(touch /tmp/x)",
            "`id`",
            "a|b",
            "a b",
            "../etc",
            "verity/../../root",
            "-rf",          // argv flag smuggling
            "verity/-e/sh", // flag in a later component
            "a\nb",
            "x>y",
        ] {
            assert!(!rel_path_is_safe(bad), "should reject {bad:?}");
        }
    }
}
