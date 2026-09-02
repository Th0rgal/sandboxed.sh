//! Head-bound evidence goes stale when the head moves.
//!
//! An `accept` receipt whose subject is `owner/repo#233@<head>` proves
//! something about *that* head. When the PR advances (a force-push, a repair
//! commit), the claim is no longer about the governed artifact: this observer
//! appends an `invalidate` receipt and the situation builder reopens the
//! track on its next read. Nothing is deleted, nothing is edited.
//!
//! Polling GitHub is the fallback; PR webhooks can call
//! [`check_receipt`] directly when they arrive.

use std::collections::HashMap;
use std::sync::Arc;

use super::projects_store::Receipt;
use super::routes::AppState;

const WATCH_INTERVAL_SECS: u64 = 600;

/// `owner/repo#233@abc123` → (`owner/repo`, 233, `abc123`).
pub fn parse_pr_subject(subject: &str) -> Option<(String, u64, String)> {
    let (repo, rest) = subject.trim().split_once('#')?;
    let (number, head) = rest.split_once('@')?;
    let number = number.trim().parse::<u64>().ok()?;
    let repo = repo.trim();
    let head = head.trim();
    if repo.is_empty() || !repo.contains('/') || head.len() < 7 {
        return None;
    }
    Some((repo.to_string(), number, head.to_string()))
}

/// Does `observed` prove the same head as the receipt? Short and full SHAs
/// compare by prefix so a 7-char handle stays valid.
pub fn same_head(recorded: &str, observed: &str) -> bool {
    let recorded = recorded.trim().to_ascii_lowercase();
    let observed = observed.trim().to_ascii_lowercase();
    !recorded.is_empty()
        && !observed.is_empty()
        && (observed.starts_with(&recorded) || recorded.starts_with(&observed))
}

async fn fetch_pr_head(
    client: &reqwest::Client,
    token: Option<&str>,
    repo: &str,
    number: u64,
) -> Result<String, String> {
    let mut request = client
        .get(format!(
            "https://api.github.com/repos/{repo}/pulls/{number}"
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "sandboxed-sh-evidence-watch");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    body.pointer("/head/sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "no head.sha in PR payload".to_string())
}

/// Compare one receipt against an observed head; invalidate on mismatch.
/// Returns true when an invalidation was appended.
pub fn check_receipt(
    state: &AppState,
    receipt: &Receipt,
    observed_head: &str,
) -> Result<bool, String> {
    let Some((_, _, recorded)) = parse_pr_subject(&receipt.subject_id) else {
        return Ok(false);
    };
    if same_head(&recorded, observed_head) {
        return Ok(false);
    }
    let Some(track_id) = receipt.track_id.as_deref() else {
        return Ok(false);
    };
    let Some((slug, key)) = state.projects.track_key_for_id(track_id)? else {
        return Ok(false);
    };
    state
        .projects
        .invalidate_track_evidence(
            &slug,
            &key,
            &receipt.id,
            &format!(
                "governed head moved from {recorded} to {observed_head}; evidence was head-bound"
            ),
            "system",
            "evidence_watch",
        )
        .map_err(|e| e.to_string())?;
    tracing::info!(
        project = %slug,
        track = %key,
        receipt = %receipt.id,
        "evidence_watch: head-bound evidence invalidated"
    );
    Ok(true)
}

/// One pass over every active PR receipt. Public so a webhook handler or a
/// test can drive it without the timer.
pub async fn run_once(state: &Arc<AppState>) -> Result<usize, String> {
    let receipts = state.projects.active_accept_receipts("pr")?;
    if receipts.is_empty() {
        return Ok(0);
    }
    let token = state
        .github_connection
        .get()
        .await
        .map(|connection| connection.access_token);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut heads: HashMap<(String, u64), Result<String, String>> = HashMap::new();
    let mut invalidated = 0usize;
    for receipt in &receipts {
        let Some((repo, number, _)) = parse_pr_subject(&receipt.subject_id) else {
            continue;
        };
        let key = (repo.clone(), number);
        if !heads.contains_key(&key) {
            let head = fetch_pr_head(&client, token.as_deref(), &repo, number).await;
            heads.insert(key.clone(), head);
        }
        match heads.get(&key) {
            Some(Ok(head)) => {
                if check_receipt(state, receipt, head)? {
                    invalidated += 1;
                }
            }
            Some(Err(error)) => {
                tracing::warn!(
                    subject = %receipt.subject_id,
                    %error,
                    "evidence_watch: source unavailable; leaving evidence as is"
                );
            }
            None => {}
        }
    }
    Ok(invalidated)
}

pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(WATCH_INTERVAL_SECS)).await;
            match super::track_leases::sweep(&state).await {
                Ok(report)
                    if report.released_terminal
                        + report.released_missing
                        + report.expired_overdue
                        > 0 =>
                {
                    tracing::info!(?report, "track leases swept")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "track lease sweep failed"),
            }
            match run_once(&state).await {
                Ok(0) => {}
                Ok(count) => {
                    tracing::info!(count, "evidence_watch: invalidated stale head evidence")
                }
                Err(error) => tracing::warn!(%error, "evidence_watch: pass failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_pr_subject_handle() {
        assert_eq!(
            parse_pr_subject("lfglabs-dev/verity#233@20494801abcdef"),
            Some(("lfglabs-dev/verity".into(), 233, "20494801abcdef".into()))
        );
        assert_eq!(parse_pr_subject("nope"), None);
        assert_eq!(parse_pr_subject("o/r#x@abc1234"), None);
        assert_eq!(
            parse_pr_subject("o/r#1@abc"),
            None,
            "too short to be a head"
        );
    }

    #[test]
    fn heads_compare_by_prefix() {
        assert!(same_head("20494801", "20494801f6e2d5d4"));
        assert!(same_head("20494801F6E2", "20494801f6e2d5d4"));
        assert!(!same_head("f75f2d5d", "20494801f6e2d5d4"));
        assert!(!same_head("", "abc"));
    }
}
