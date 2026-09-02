//! One-shot import of Hermes tracker Markdown into `project_tracks`.
//!
//! The tracker files (`HERMES_PROJECTS_DIR/<slug>.md`) carried the plan as a
//! `## Deliverables` checklist. That was a second writable truth; after the
//! import they are narrative only. The importer:
//!
//! - parses `- [ ] CODE title …` / `- [x] …` lines (any section);
//! - creates missing tracks with `origin = imported`, never overwrites a
//!   declared title, and records every code spelling as an alias;
//! - turns checked items into `legacy_import` claim receipts (claim only —
//!   the situation builder never counts them as verified);
//! - records PR references as track refs (matching hints, not merges);
//! - reports ambiguities instead of guessing (an item without a code prefix,
//!   a code that maps onto an existing track with a different title);
//! - is idempotent on `(project, source_path, source_hash, parser_version)`.
//!
//! Every correction lands in one immutable `reconcile` receipt per run.

use std::sync::Arc;

use axum::{extract::Path as AxumPath, extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::projects_store::{
    normalize_track_key, NewReceipt, ProjectsStore, TRACKER_PARSER_VERSION,
};
use super::routes::AppState;

pub const PARSER_VERSION: i64 = TRACKER_PARSER_VERSION;

/// One checklist line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackerItem {
    /// Normalized key of the first code (`a1`), or a title-derived key when
    /// the line has no code prefix.
    pub key: String,
    /// Every code on the line in source spelling (`A1`, `A2` for `A1/A2`).
    pub codes: Vec<String>,
    pub title: String,
    pub checked: bool,
    pub pr_refs: Vec<i64>,
    /// 1-based line number in the source.
    pub line: usize,
    /// Why this item needs a human look, if it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambiguous: Option<String>,
}

fn is_code(token: &str) -> bool {
    // `S0`, `UX1`, `A1`, `P-ETH-1`, `E2`: letters, optional dash groups,
    // ending in digits; short.
    let token = token.trim_end_matches([':', '.', ')']);
    if token.len() > 12 || token.is_empty() {
        return false;
    }
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let last_is_digit = token
        .chars()
        .last()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    has_digit
        && last_is_digit
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && token.chars().filter(|c| c.is_ascii_alphabetic()).count() <= 6
}

fn key_from_title(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().take(5).collect();
    let key = normalize_track_key(&words.join("-"));
    key.chars()
        .take(40)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

fn pr_refs(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find('#') {
        let after = &rest[index + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let before = rest[..index].trim_end();
        let is_pr = before.ends_with("PR")
            || before.ends_with("pr")
            || before.ends_with("PRs")
            || before.ends_with("pull request")
            || before.ends_with("Pull Request");
        if is_pr && !digits.is_empty() {
            if let Ok(number) = digits.parse::<i64>() {
                if !out.contains(&number) {
                    out.push(number);
                }
            }
        }
        rest = after;
    }
    out
}

/// Parse every checkbox line. Parser version [`PARSER_VERSION`].
pub fn parse_tracker(content: &str) -> Vec<TrackerItem> {
    let mut items = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim_start();
        let Some(rest) = line
            .strip_prefix("- [")
            .or_else(|| line.strip_prefix("* ["))
        else {
            continue;
        };
        let Some((mark, rest)) = rest.split_once(']') else {
            continue;
        };
        let checked = match mark.trim() {
            "x" | "X" => true,
            "" => false,
            _ => continue,
        };
        let text = rest.trim();
        if text.is_empty() {
            continue;
        }
        let mut words = text.splitn(2, char::is_whitespace);
        let first = words.next().unwrap_or("");
        let first_clean = first.trim_end_matches([':', '.', ')']);
        let (codes, ambiguous, key) = if first_clean.split('/').all(is_code) {
            let codes: Vec<String> = first_clean.split('/').map(str::to_string).collect();
            let key = normalize_track_key(&codes[0]);
            (codes, None, key)
        } else {
            (
                Vec::new(),
                Some("no code prefix; key derived from the title".to_string()),
                key_from_title(text),
            )
        };
        if key.is_empty() {
            continue;
        }
        items.push(TrackerItem {
            key,
            codes,
            title: text
                .chars()
                .take(240)
                .collect::<String>()
                .trim()
                .to_string(),
            checked,
            pr_refs: pr_refs(text),
            line: index + 1,
            ambiguous,
        });
    }
    items
}

pub fn source_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    /// Where the content came from; recorded on receipts, never re-read.
    pub source_path: String,
    pub content: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportedItem {
    pub key: String,
    pub title: String,
    pub checked: bool,
    /// `created` | `existing` | `existing_claimed`
    pub action: &'static str,
    pub claim_added: bool,
    pub aliases: Vec<String>,
    pub pr_refs: Vec<i64>,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub slug: String,
    pub source_path: String,
    pub source_hash: String,
    pub parser_version: i64,
    pub dry_run: bool,
    /// The same (source, hash, parser) was already imported: nothing written.
    pub already_imported: bool,
    pub items: Vec<ImportedItem>,
    pub ambiguities: Vec<serde_json::Value>,
    pub created: usize,
    pub claims: usize,
    pub proposals_imported: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
}

/// Plan the import against the store without writing. Shared by dry-run and
/// the real run so both report the same actions.
fn plan_import(
    store: &ProjectsStore,
    slug: &str,
    items: &[TrackerItem],
) -> Result<(Vec<ImportedItem>, Vec<serde_json::Value>), String> {
    let mut planned = Vec::with_capacity(items.len());
    let mut ambiguities = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();
    for item in items {
        if let Some(reason) = &item.ambiguous {
            ambiguities.push(serde_json::json!({
                "line": item.line,
                "key": item.key,
                "title": item.title,
                "reason": reason,
            }));
        }
        // Every code on the line is its own track (`A1/A2` were split by the
        // operator on the board already); the extra codes carry the same
        // title and get flagged.
        let keys: Vec<(String, Vec<String>)> = if item.codes.len() > 1 {
            ambiguities.push(serde_json::json!({
                "line": item.line,
                "codes": item.codes,
                "reason": "several codes on one line; imported as separate tracks with the same title",
            }));
            item.codes
                .iter()
                .map(|code| (normalize_track_key(code), vec![code.clone()]))
                .collect()
        } else {
            vec![(item.key.clone(), item.codes.clone())]
        };
        for (key, aliases) in keys {
            if !seen_keys.insert(key.clone()) {
                ambiguities.push(serde_json::json!({
                    "line": item.line,
                    "key": key,
                    "reason": "duplicate key in the same file; only the first line is imported",
                }));
                continue;
            }
            let existing = store.track(slug, &key)?;
            let (action, claim_added) = match &existing {
                None => ("created", item.checked),
                Some(track) if track.claim.is_some() => ("existing_claimed", false),
                Some(track) => {
                    if let Some(title) = track.title.as_deref() {
                        if !title.eq_ignore_ascii_case(&item.title)
                            && !item.title.starts_with(title)
                        {
                            ambiguities.push(serde_json::json!({
                                "line": item.line,
                                "key": key,
                                "declared_title": title,
                                "markdown_title": item.title,
                                "reason": "key already declared with a different title; declared title kept",
                            }));
                        }
                    }
                    ("existing", item.checked && track.lifecycle == "active")
                }
            };
            planned.push(ImportedItem {
                key,
                title: item.title.clone(),
                checked: item.checked,
                action,
                claim_added,
                aliases,
                pr_refs: item.pr_refs.clone(),
                line: item.line,
            });
        }
    }
    Ok((planned, ambiguities))
}

/// `POST /api/projects/:slug/imports`
pub async fn import_tracker(
    State(state): State<Arc<AppState>>,
    AxumPath(requested): AxumPath<String>,
    Json(req): Json<ImportRequest>,
) -> Result<Json<ImportReport>, (StatusCode, String)> {
    if !super::projects_overview::is_plain_key(&requested) {
        return Err((StatusCode::BAD_REQUEST, "invalid project slug".to_string()));
    }
    let Some(slug) = super::projects_overview::resolve_roster_slug(&state.projects, &requested)
        .map_err(internal)?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown project '{requested}'"),
        ));
    };
    let report = run_import(
        &state.projects,
        &slug,
        &req.source_path,
        &req.content,
        req.dry_run,
        req.actor.as_deref().unwrap_or("palomactl"),
    )
    .map_err(internal)?;
    Ok(Json(report))
}

fn internal(error: String) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error)
}

pub fn run_import(
    store: &ProjectsStore,
    slug: &str,
    source_path: &str,
    content: &str,
    dry_run: bool,
    actor: &str,
) -> Result<ImportReport, String> {
    let hash = source_hash(content);
    let already = store.import_exists(slug, source_path, &hash, PARSER_VERSION)?;
    let items = parse_tracker(content);
    let (planned, ambiguities) = plan_import(store, slug, &items)?;
    let mut report = ImportReport {
        slug: slug.to_string(),
        source_path: source_path.to_string(),
        source_hash: hash.clone(),
        parser_version: PARSER_VERSION,
        dry_run,
        already_imported: already,
        created: planned.iter().filter(|p| p.action == "created").count(),
        claims: planned.iter().filter(|p| p.claim_added).count(),
        proposals_imported: 0,
        items: planned,
        ambiguities,
        receipt_id: None,
    };
    if dry_run || already {
        return Ok(report);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut corrections: Vec<serde_json::Value> = Vec::new();
    report.proposals_imported = store.import_open_proposals(slug)?;
    if report.proposals_imported > 0 {
        corrections.push(serde_json::json!({
            "op": "proposals_imported",
            "count": report.proposals_imported,
        }));
    }
    for item in &report.items {
        if item.action == "created" {
            store.import_track(slug, &item.key, &item.title, &item.aliases)?;
            corrections.push(serde_json::json!({
                "op": "imported",
                "track": item.key,
                "source": format!("{source_path}#L{}", item.line),
            }));
        } else {
            for alias in &item.aliases {
                store.add_track_alias(slug, &item.key, alias, "imported_code")?;
            }
        }
        for number in &item.pr_refs {
            store.add_track_ref(slug, &item.key, "pr", None, *number, None)?;
        }
        if item.claim_added {
            store
                .insert_receipt(&NewReceipt {
                    idempotency_key: format!("legacy_import:{slug}:{}:{hash}", item.key),
                    kind: "legacy_import".into(),
                    project_slug: Some(slug.to_string()),
                    track_id: store.track(slug, &item.key)?.map(|track| track.id),
                    criterion_id: None,
                    subject_type: "import".into(),
                    subject_id: format!("{source_path}#L{}", item.line),
                    outcome: "observed".into(),
                    actor_type: "operator".into(),
                    actor_id: actor.to_string(),
                    verifier: None,
                    supersedes_receipt_id: None,
                    observed_at: now.clone(),
                    payload: serde_json::json!({
                        "title": item.title,
                        "source_path": source_path,
                        "source_hash": hash,
                        "parser_version": PARSER_VERSION,
                        "line": item.line,
                        "pr_refs": item.pr_refs,
                        "note": "checked in tracker Markdown before receipts existed; claim only",
                    }),
                })
                .map_err(|e| e.to_string())?;
            corrections.push(serde_json::json!({
                "op": "legacy_checked_to_claim_only",
                "track": item.key,
                "source": format!("{source_path}#L{}", item.line),
            }));
        }
    }
    for ambiguity in &report.ambiguities {
        corrections.push(serde_json::json!({
            "op": "ambiguity",
            "detail": ambiguity,
        }));
    }
    let receipt = store
        .insert_receipt(&NewReceipt {
            idempotency_key: format!("reconcile:import:{slug}:{hash}:{PARSER_VERSION}"),
            kind: "reconcile".into(),
            project_slug: Some(slug.to_string()),
            track_id: None,
            criterion_id: None,
            subject_type: "import".into(),
            subject_id: source_path.to_string(),
            outcome: "observed".into(),
            actor_type: "operator".into(),
            actor_id: actor.to_string(),
            verifier: None,
            supersedes_receipt_id: None,
            observed_at: now,
            payload: serde_json::json!({
                "source_path": source_path,
                "source_hash": hash,
                "parser_version": PARSER_VERSION,
                "corrections": corrections,
            }),
        })
        .map_err(|e| e.to_string())?;
    store.record_import(
        slug,
        source_path,
        &hash,
        PARSER_VERSION,
        report.items.len(),
        Some(&receipt.id),
    )?;
    report.receipt_id = Some(receipt.id);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIDO: &str = r#"# Lido SRv3 final-deliverable fidelity

## Deliverables
- [x] S0 P-DEREF-1 removal, no active mention remains (PR #215)
- [x] A1/A2 strengthened P-ALLOC-1/P-ALLOC-2, no P-ALLOC-3 (PR #216)
- [ ] UX1 Lido acceptance pass (P0 before further semantic widening)
- [ ] UX2 deploy and verify the corrected report/map on the client-visible site
- [ ] Oracle final fidelity closure
- [x] UX1 duplicate line that must not import twice

## Active missions
- `715e3317-99e1-4f69-b5e0-1ba563e0fdac` — not a checkbox
"#;

    #[test]
    fn parses_codes_checkboxes_and_pr_refs() {
        let items = parse_tracker(LIDO);
        assert_eq!(items.len(), 6);
        assert_eq!(items[0].key, "s0");
        assert!(items[0].checked);
        assert_eq!(items[0].pr_refs, vec![215]);
        assert_eq!(items[1].codes, vec!["A1", "A2"]);
        assert_eq!(items[2].key, "ux1");
        assert!(!items[2].checked);
        let oracle = &items[4];
        assert_eq!(oracle.key, "oracle-final-fidelity-closure");
        assert!(oracle.ambiguous.is_some());
        assert!(items.iter().all(|item| !item.title.is_empty()));
    }

    #[test]
    fn import_is_idempotent_and_reports_everything() {
        let store = ProjectsStore::open_in_memory().expect("store");
        store
            .upsert_project("verity-lido", Some("Lido"), None, None, None)
            .expect("seed");
        // A track the operator already declared with a different title.
        store
            .patch_track(
                "verity-lido",
                "UX1",
                None,
                Some("open"),
                Some("UX1 acceptance"),
                None,
                None,
            )
            .expect("declared");

        let dry = run_import(
            &store,
            "verity-lido",
            "active/verity-lido.md",
            LIDO,
            true,
            "t",
        )
        .expect("dry run");
        assert!(dry.dry_run);
        assert_eq!(
            store.tracks("verity-lido").unwrap().len(),
            1,
            "dry run writes nothing"
        );
        assert_eq!(dry.created, 5, "s0, a1, a2, ux2, oracle");

        let real = run_import(
            &store,
            "verity-lido",
            "active/verity-lido.md",
            LIDO,
            false,
            "t",
        )
        .expect("import");
        assert!(
            real.ambiguities.iter().any(|a| a["reason"].as_str()
                == Some("key already declared with a different title; declared title kept")),
            "the UX1 title mismatch is reported, not resolved"
        );

        let again = run_import(
            &store,
            "verity-lido",
            "active/verity-lido.md",
            LIDO,
            false,
            "t",
        )
        .expect("again");
        assert!(again.already_imported);
        assert_eq!(store.tracks("verity-lido").unwrap().len(), 6);
        let receipts: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
            .unwrap();
        // 3 claims + 1 reconcile from the real run; nothing from the re-run.
        assert_eq!(receipts, 4);
    }
}
