//! Global control session API (interactive, queued).
//!
//! This module implements a single global "control session" that:
//! - accepts user messages at any time (queued FIFO)
//! - runs a persistent root-agent conversation sequentially
//! - streams structured events via SSE (Tool UI friendly)
//! - supports frontend/interactive tools by accepting tool results
//! - supports persistent missions (goal-oriented sessions)

use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::stream::Stream;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentContext, AgentRef, TerminalReason};
use crate::config::Config;
use crate::mcp::McpRegistry;
use crate::secrets::SecretsStore;
use crate::util::{build_history_context, internal_error};
use crate::workspace;

use super::auth::AuthUser;
use super::desktop;
use super::library::SharedLibrary;
use super::mission_store::{
    self, create_mission_store, now_string, Mission, MissionHistoryEntry, MissionStore,
    MissionStoreType,
};
use super::routes::AppState;

const SERVER_SHUTDOWN_AUTO_RESUME_MAX_AGE_HOURS: u64 = 48;
const INTERRUPTED_RESUME_PROMPT: &str = "You were interrupted, resume your work.";

/// Returns a safe index to truncate a string at, ensuring we don't cut UTF-8 characters.
pub(super) fn safe_truncate_index(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    // Find a char boundary at or before max
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Derive a human-readable activity label from a tool call.
fn activity_label_from_tool_call(tool_name: &str, args: &serde_json::Value) -> String {
    fn extract_str<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
        for key in keys {
            if let Some(v) = args.get(*key).and_then(|v| v.as_str()) {
                return Some(v);
            }
        }
        None
    }

    fn basename(path: &str) -> &str {
        std::path::Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(path)
    }

    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            let end = safe_truncate_index(s, max);
            format!("{}…", &s[..end])
        }
    }

    match tool_name {
        "Bash" | "bash" => {
            let cmd = extract_str(args, &["command"]).unwrap_or("…");
            let first_line = cmd.lines().next().unwrap_or(cmd);
            format!("Running: {}", truncate(first_line, 60))
        }
        "Read" | "read_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Reading: {}", basename(path))
        }
        "Edit" | "edit_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Editing: {}", basename(path))
        }
        "Write" | "write_file" => {
            let path = extract_str(args, &["file_path", "path"]).unwrap_or("…");
            format!("Writing: {}", basename(path))
        }
        "Grep" | "grep" | "search" => {
            let pattern = extract_str(args, &["pattern"]).unwrap_or("…");
            format!("Searching: {}", truncate(pattern, 40))
        }
        "Glob" | "glob" => {
            let pattern = extract_str(args, &["pattern"]).unwrap_or("…");
            format!("Finding: {}", truncate(pattern, 50))
        }
        "WebSearch" | "web_search" => {
            let query = extract_str(args, &["query"]).unwrap_or("…");
            format!("Searching web: {}", truncate(query, 40))
        }
        "WebFetch" | "web_fetch" => "Fetching web page".to_string(),
        "Task" | "delegate_task" => {
            let desc = extract_str(args, &["description", "prompt", "subject"]).unwrap_or("…");
            format!("Subtask: {}", truncate(desc, 80))
        }
        "TaskCreate" => {
            let desc = extract_str(args, &["subject", "description"]).unwrap_or("…");
            format!("Creating task: {}", truncate(desc, 80))
        }
        "Skill" => {
            let skill = extract_str(args, &["skill"]).unwrap_or("…");
            format!("Running skill: {}", skill)
        }
        "AskUserQuestion" => "Waiting for input".to_string(),
        "NotebookEdit" => {
            let path = extract_str(args, &["notebook_path"]).unwrap_or("…");
            format!("Editing notebook: {}", basename(path))
        }
        name if name.starts_with("mcp__") => {
            let parts: Vec<&str> = name.splitn(3, "__").collect();
            if parts.len() == 3 {
                format!("{}: {}", parts[1], parts[2])
            } else {
                format!("Tool: {}", name)
            }
        }
        other => format!("Tool: {}", other),
    }
}

/// Extract a concise title from the assistant's first response.
/// Returns the first substantive line, cleaned of markdown formatting.
fn extract_title_from_assistant(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut inside_fenced_block: Option<char> = None;
    let mut first_line: Option<&str> = None;
    for idx in 0..lines.len() {
        let line = lines[idx].trim();
        if let Some(fence_char) = markdown_fence_char(line) {
            if inside_fenced_block == Some(fence_char) {
                inside_fenced_block = None;
                continue;
            }
            if inside_fenced_block.is_none() {
                let has_closing_fence = lines[(idx + 1)..]
                    .iter()
                    .any(|candidate| markdown_fence_char(candidate.trim()) == Some(fence_char));
                if has_closing_fence {
                    inside_fenced_block = Some(fence_char);
                }
                continue;
            }
        }
        if inside_fenced_block.is_none() && !line.is_empty() {
            first_line = Some(line);
            break;
        }
    }
    let first_line = first_line?;

    // Strip markdown prefixes
    let cleaned = first_line.trim_start_matches(['#', '*', '-', ' ']).trim();

    if cleaned.len() < 5 {
        return None;
    }
    if is_unsuccessful_assistant_summary(cleaned) {
        return None;
    }

    let max_len = cleaned.len().min(100);
    let safe_end = safe_truncate_index(cleaned, max_len);
    if safe_end < cleaned.len() {
        Some(format!("{}...", &cleaned[..safe_end]))
    } else {
        Some(cleaned.to_string())
    }
}

fn is_unsuccessful_assistant_summary(text: &str) -> bool {
    let normalized = normalize_metadata_text(text);
    if normalized.is_empty() {
        return true;
    }
    normalized.starts_with("error")
        || normalized.starts_with("failed")
        || normalized.starts_with("exception")
        || normalized.starts_with("traceback")
        || normalized.starts_with("i am sorry")
        || normalized.starts_with("im sorry")
        || normalized.starts_with("sorry")
}

/// Extract a concise short description from the first user or assistant message.
fn extract_short_description_from_history(
    history: &[(String, String)],
    max_len: usize,
) -> Option<String> {
    history
        .iter()
        .find(|(role, _)| role == "user")
        .and_then(|(_, content)| extract_short_description_from_content(content, max_len))
        .or_else(|| {
            history
                .iter()
                .find(|(role, _)| role == "assistant")
                .and_then(|(_, content)| extract_short_description_from_content(content, max_len))
        })
}

fn extract_short_description_from_recent_role(
    history: &[(String, String)],
    role: &str,
    max_len: usize,
) -> Option<String> {
    history
        .iter()
        .rev()
        .filter(|(entry_role, _)| entry_role == role)
        .find_map(|(_, content)| extract_short_description_from_content(content, max_len))
}

fn extract_short_description_from_recent_history(
    history: &[(String, String)],
    max_len: usize,
) -> Option<String> {
    extract_short_description_from_recent_role(history, "assistant", max_len)
        .or_else(|| extract_short_description_from_recent_role(history, "user", max_len))
}

fn extract_short_description_from_first_successful_assistant(
    history: &[(String, String)],
    max_len: usize,
) -> Option<String> {
    history
        .iter()
        .filter(|(entry_role, _)| entry_role == "assistant")
        .find_map(|(_, content)| {
            extract_short_description_from_content(content, max_len)
                .filter(|candidate| !is_unsuccessful_assistant_summary(candidate))
        })
}

fn assistant_reply_is_successful(content: &str) -> bool {
    extract_short_description_from_content(content, 160)
        .map(|candidate| !is_unsuccessful_assistant_summary(&candidate))
        .unwrap_or(false)
}

fn is_terminal_mission_status(status: &MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Completed
            | MissionStatus::Failed
            | MissionStatus::Interrupted
            | MissionStatus::Blocked
            | MissionStatus::NotFeasible
    )
}

fn should_persist_synthetic_thought(candidate: &str, assistant_content: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }

    let assistant_content = assistant_content.trim();
    if assistant_content.is_empty() {
        return true;
    }

    assistant_content != candidate && !assistant_content.starts_with(candidate)
}

fn merge_pending_text_delta(existing: &mut String, content: &str) {
    if content.starts_with(existing.as_str()) {
        existing.clear();
        existing.push_str(content);
    } else {
        existing.push_str(content);
    }
}

fn synthetic_thought_from_text_delta(
    pending_text_deltas: &mut HashMap<Uuid, String>,
    mission_id: Uuid,
    event: &AgentEvent,
) -> Option<String> {
    match event {
        AgentEvent::TextDelta { content, .. } => {
            if content.trim().is_empty() {
                pending_text_deltas.remove(&mission_id);
            } else {
                pending_text_deltas
                    .entry(mission_id)
                    .and_modify(|existing| merge_pending_text_delta(existing, content))
                    .or_insert_with(|| content.clone());
            }
            None
        }
        AgentEvent::Thinking { .. } => {
            pending_text_deltas.remove(&mission_id);
            None
        }
        AgentEvent::ToolCall { .. } | AgentEvent::UserMessage { .. } | AgentEvent::Error { .. } => {
            pending_text_deltas.remove(&mission_id)
        }
        AgentEvent::AssistantMessage { content, .. } => pending_text_deltas
            .remove(&mission_id)
            .filter(|candidate| should_persist_synthetic_thought(candidate, content)),
        AgentEvent::MissionStatusChanged { status, .. } if is_terminal_mission_status(status) => {
            pending_text_deltas.remove(&mission_id)
        }
        _ => None,
    }
}

fn extract_short_description_from_content(content: &str, max_len: usize) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut inside_fenced_block: Option<char> = None;
    let mut first_line: Option<&str> = None;
    for idx in 0..lines.len() {
        let line = lines[idx].trim();
        if let Some(fence_char) = markdown_fence_char(line) {
            if inside_fenced_block == Some(fence_char) {
                inside_fenced_block = None;
                continue;
            }
            if inside_fenced_block.is_none() {
                let has_closing_fence = lines[(idx + 1)..]
                    .iter()
                    .any(|candidate| markdown_fence_char(candidate.trim()) == Some(fence_char));
                if has_closing_fence {
                    inside_fenced_block = Some(fence_char);
                }
                continue;
            }
        }
        if inside_fenced_block.is_none() && !line.is_empty() {
            first_line = Some(line);
            break;
        }
    }
    let first_line = first_line?;

    let cleaned = strip_markdown_prefixes(first_line);
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    let max_len = collapsed.len().min(max_len);
    let safe_end = safe_truncate_index(&collapsed, max_len);
    if safe_end < collapsed.len() {
        Some(format!("{}...", &collapsed[..safe_end]))
    } else {
        Some(collapsed)
    }
}

fn strip_markdown_prefixes(line: &str) -> &str {
    let mut cleaned = line.trim();
    loop {
        let next = if let Some(stripped) = cleaned.strip_prefix('#') {
            Some(stripped.trim_start())
        } else if let Some(stripped) = cleaned.strip_prefix('>') {
            Some(stripped.trim_start())
        } else if let Some(stripped) = cleaned.strip_prefix("- ") {
            Some(stripped.trim_start())
        } else if let Some(stripped) = cleaned.strip_prefix("* ") {
            Some(stripped.trim_start())
        } else if let Some(stripped) = cleaned.strip_prefix("+ ") {
            Some(stripped.trim_start())
        } else {
            strip_ordered_list_prefix(cleaned)
        };

        match next {
            Some(candidate) if candidate != cleaned => cleaned = candidate,
            _ => break,
        }
    }
    cleaned
}

fn strip_ordered_list_prefix(line: &str) -> Option<&str> {
    let mut idx = 0;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            idx += ch.len_utf8();
        } else {
            break;
        }
    }
    if idx == 0 || idx >= line.len() {
        return None;
    }

    let marker = line[idx..].chars().next()?;
    if marker != '.' && marker != ')' {
        return None;
    }
    let marker_end = idx + marker.len_utf8();
    let suffix = line.get(marker_end..)?;
    if suffix.is_empty() {
        return None;
    }
    if !suffix.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    Some(suffix.trim_start())
}

fn markdown_fence_char(line: &str) -> Option<char> {
    if line.starts_with("```") {
        Some('`')
    } else if line.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn normalize_metadata_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const METADATA_SOURCE_BACKEND_HEURISTIC: &str = "backend_heuristic";
const METADATA_SOURCE_USER: &str = "user";
const METADATA_VERSION_V1: &str = "v1";
static MISSION_TITLE_UPDATE_LOCK: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));
struct MetadataRefreshTaskEntry {
    handle: tokio::task::JoinHandle<()>,
    force_refresh: bool,
    task_id: u64,
}

struct MetadataRefreshTaskRegistration {
    superseded: Option<tokio::task::JoinHandle<()>>,
}

static MISSION_METADATA_REFRESH_TASKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<Uuid, MetadataRefreshTaskEntry>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
static MISSION_METADATA_REFRESH_TASK_ID: AtomicU64 = AtomicU64::new(1);
static MISSION_METADATA_REFRESH_BASELINES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<Uuid, usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn register_metadata_refresh_task(
    tasks: &mut HashMap<Uuid, MetadataRefreshTaskEntry>,
    mission_id: Uuid,
    force_refresh: bool,
    task_id: u64,
    handle: tokio::task::JoinHandle<()>,
) -> MetadataRefreshTaskRegistration {
    tasks.retain(|_, existing| !existing.handle.is_finished());

    if let Some(existing) = tasks.get(&mission_id) {
        if existing.force_refresh && !force_refresh {
            return MetadataRefreshTaskRegistration {
                superseded: Some(handle),
            };
        }
    }

    let replaced = tasks.insert(
        mission_id,
        MetadataRefreshTaskEntry {
            handle,
            force_refresh,
            task_id,
        },
    );
    MetadataRefreshTaskRegistration {
        superseded: replaced.map(|entry| entry.handle),
    }
}

fn should_skip_metadata_refresh_schedule(
    tasks: &mut HashMap<Uuid, MetadataRefreshTaskEntry>,
    mission_id: Uuid,
    force_refresh: bool,
) -> bool {
    tasks.retain(|_, existing| !existing.handle.is_finished());
    matches!(
        tasks.get(&mission_id),
        Some(existing) if existing.force_refresh && !force_refresh
    )
}

fn complete_metadata_refresh_task(
    tasks: &mut HashMap<Uuid, MetadataRefreshTaskEntry>,
    mission_id: Uuid,
    task_id: u64,
) {
    if tasks
        .get(&mission_id)
        .map(|entry| entry.task_id == task_id)
        .unwrap_or(false)
    {
        tasks.remove(&mission_id);
    }
}

fn clear_mission_metadata_refresh_state(mission_id: Uuid) {
    let stale_task = {
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        tasks.remove(&mission_id)
    };
    if let Some(stale_task) = stale_task {
        stale_task.handle.abort();
    }

    let mut baselines = MISSION_METADATA_REFRESH_BASELINES
        .lock()
        .expect("metadata refresh baseline lock poisoned");
    baselines.remove(&mission_id);
}

async fn clear_stale_mission_metadata_refresh_state(mission_store: &Arc<dyn MissionStore>) {
    let tracked_ids: std::collections::HashSet<Uuid> = {
        let tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        let baselines = MISSION_METADATA_REFRESH_BASELINES
            .lock()
            .expect("metadata refresh baseline lock poisoned");

        tasks.keys().chain(baselines.keys()).copied().collect()
    };

    for mission_id in tracked_ids {
        match mission_store.get_mission(mission_id).await {
            Ok(Some(_)) => {}
            Ok(None) => clear_mission_metadata_refresh_state(mission_id),
            Err(err) => tracing::warn!(
                "Failed to verify mission {} while clearing stale metadata refresh state: {}",
                mission_id,
                err
            ),
        }
    }
}

fn normalize_raw_title_for_dedupe(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn strip_numeric_title_suffix(title: &str) -> &str {
    let trimmed = title.trim();
    if !trimmed.ends_with(')') {
        return trimmed;
    }
    let Some(open_idx) = trimmed.rfind(" (") else {
        return trimmed;
    };
    let suffix = &trimmed[(open_idx + 2)..(trimmed.len() - 1)];
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return trimmed;
    }
    trimmed[..open_idx].trim_end()
}

fn canonical_title_key(title: &str) -> String {
    normalize_metadata_text(strip_numeric_title_suffix(title))
}

fn title_similarity_token(token: &str) -> String {
    if token.len() > 4 && token.ends_with("ies") {
        return format!("{}y", &token[..token.len() - 3]);
    }
    if token.len() > 4 && token.ends_with('s') && !token.ends_with("ss") {
        return token[..token.len() - 1].to_string();
    }
    token.to_string()
}

fn title_similarity_tokens(title: &str) -> Vec<String> {
    normalize_metadata_text(strip_numeric_title_suffix(title))
        .split_whitespace()
        .filter(|token| !token.is_empty() && !is_search_stopword(token))
        .map(title_similarity_token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn token_similarity_strength(lhs: &str, rhs: &str) -> f64 {
    search_token_match_strength(lhs, rhs).max(search_token_match_strength(rhs, lhs))
}

fn title_near_duplicate_score(lhs: &str, rhs: &str) -> f64 {
    let lhs_tokens = title_similarity_tokens(lhs);
    let rhs_tokens = title_similarity_tokens(rhs);
    if lhs_tokens.is_empty() || rhs_tokens.is_empty() {
        return 0.0;
    }

    let lhs_best_avg = lhs_tokens
        .iter()
        .map(|lhs_token| {
            rhs_tokens
                .iter()
                .map(|rhs_token| token_similarity_strength(lhs_token, rhs_token))
                .fold(0.0_f64, f64::max)
        })
        .sum::<f64>()
        / lhs_tokens.len() as f64;

    let rhs_best_avg = rhs_tokens
        .iter()
        .map(|rhs_token| {
            lhs_tokens
                .iter()
                .map(|lhs_token| token_similarity_strength(rhs_token, lhs_token))
                .fold(0.0_f64, f64::max)
        })
        .sum::<f64>()
        / rhs_tokens.len() as f64;

    (lhs_best_avg + rhs_best_avg) / 2.0
}

fn is_near_duplicate_title(candidate: &str, existing: &str) -> bool {
    const NEAR_DUPLICATE_THRESHOLD: f64 = 0.9;
    title_near_duplicate_score(candidate, existing) >= NEAR_DUPLICATE_THRESHOLD
}

fn title_collides_with_existing(candidate: &str, existing: &str) -> bool {
    let candidate_normalized = normalize_metadata_text(candidate);
    let existing_normalized = normalize_metadata_text(existing);
    if !candidate_normalized.is_empty() && candidate_normalized == existing_normalized {
        return true;
    }
    is_near_duplicate_title(candidate, existing)
}

async fn load_recent_mission_titles(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
) -> Vec<String> {
    const RECENT_TITLE_SCAN_PAGE_SIZE: usize = 200;
    const RECENT_TITLE_SCAN_MAX: usize = 2_000;

    let mut titles = Vec::new();
    let mut offset = 0;

    while offset < RECENT_TITLE_SCAN_MAX {
        match mission_store
            .list_missions(RECENT_TITLE_SCAN_PAGE_SIZE, offset)
            .await
        {
            Ok(missions) => {
                if missions.is_empty() {
                    break;
                }
                let page_len = missions.len();
                titles.extend(
                    missions
                        .into_iter()
                        .filter(|mission| mission.id != mission_id)
                        .filter_map(|mission| mission.title)
                        .map(|title| title.trim().to_string())
                        .filter(|title| !title.is_empty()),
                );

                if page_len < RECENT_TITLE_SCAN_PAGE_SIZE {
                    break;
                }
                offset += RECENT_TITLE_SCAN_PAGE_SIZE;
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to load recent mission titles for metadata generation: {err}"
                );
                break;
            }
        }
    }

    if offset >= RECENT_TITLE_SCAN_MAX {
        tracing::warn!(
            "Recent-title scan hit cap ({} missions); some older titles may be skipped",
            RECENT_TITLE_SCAN_MAX
        );
    }

    titles
}

fn derive_title_qualifier_from_text(base_title: &str, text: &str) -> Option<String> {
    let base_tokens: HashSet<String> = normalize_metadata_text(base_title)
        .split_whitespace()
        .map(ToString::to_string)
        .collect();
    let qualifier_tokens: Vec<String> = normalize_metadata_text(text)
        .split_whitespace()
        .filter(|token| !base_tokens.contains(*token))
        .filter(|token| !is_search_stopword(token))
        .take(4)
        .map(ToString::to_string)
        .collect();

    if qualifier_tokens.len() < 2 {
        return None;
    }
    Some(qualifier_tokens.join(" "))
}

fn append_title_qualifier(base_title: &str, qualifier: &str) -> String {
    let combined = format!("{} - {}", base_title.trim(), qualifier.trim());
    let max_len = combined.len().min(100);
    let safe_end = safe_truncate_index(&combined, max_len);
    if safe_end < combined.len() {
        format!("{}...", &combined[..safe_end])
    } else {
        combined
    }
}

async fn diversify_generated_title_with_recent_context(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    title_candidate: String,
    history: &[(String, String)],
    fallback_user_content: Option<&str>,
) -> String {
    let recent_titles = load_recent_mission_titles(mission_store, mission_id).await;
    if recent_titles.is_empty()
        || !recent_titles
            .iter()
            .any(|existing| title_collides_with_existing(&title_candidate, existing))
    {
        return title_candidate;
    }

    // Prefer user-origin context as negative prompt signal to diversify generic duplicates.
    let mut qualifier_sources: Vec<&str> = Vec::new();
    if let Some(user_content) = fallback_user_content {
        qualifier_sources.push(user_content);
    }
    for (role, content) in history {
        if role == "user" {
            qualifier_sources.push(content);
        }
    }
    for (role, content) in history {
        if role == "assistant" {
            qualifier_sources.push(content);
        }
    }

    for source in qualifier_sources {
        if let Some(qualifier) = derive_title_qualifier_from_text(&title_candidate, source) {
            let diversified = append_title_qualifier(&title_candidate, &qualifier);
            if !recent_titles
                .iter()
                .any(|existing| title_collides_with_existing(&diversified, existing))
            {
                return diversified;
            }
        }
    }

    title_candidate
}

fn has_significant_metadata_drift(existing: &str, candidate: &str) -> bool {
    let existing_normalized = normalize_metadata_text(existing);
    let candidate_normalized = normalize_metadata_text(candidate);

    if existing_normalized.is_empty() {
        return !candidate_normalized.is_empty();
    }
    if existing_normalized == candidate_normalized {
        return false;
    }
    if existing_normalized.contains(&candidate_normalized)
        || candidate_normalized.contains(&existing_normalized)
    {
        return false;
    }

    let existing_tokens: Vec<&str> = existing_normalized.split_whitespace().collect();
    let candidate_tokens: Vec<&str> = candidate_normalized.split_whitespace().collect();
    if existing_tokens.is_empty() || candidate_tokens.is_empty() {
        return true;
    }
    let existing_set: HashSet<&str> = existing_tokens.iter().copied().collect();
    let candidate_set: HashSet<&str> = candidate_tokens.iter().copied().collect();
    let overlap = existing_set.intersection(&candidate_set).count();
    let min_len = existing_set.len().min(candidate_set.len());
    if min_len > 0 {
        let overlap_ratio = overlap as f64 / min_len as f64;
        if overlap_ratio >= 0.8 {
            return false;
        }
    }

    true
}

fn mission_search_synonyms(token: &str) -> &'static [&'static str] {
    match token {
        "api" => &["endpoint", "http", "rest", "rpc"],
        "auth" => &["login", "signin", "oauth", "credential", "credentials"],
        "blocked" => &["stalled", "waiting"],
        "bug" => &["issue", "error", "fix", "problem"],
        "ci" => &["pipeline", "build", "integration", "tests"],
        "crash" => &["panic", "exception", "failure"],
        "db" => &["database", "sql", "sqlite", "postgres"],
        "cd" => &["deploy", "release", "rollout", "ship"],
        "deploy" => &["release", "rollout", "ship"],
        "error" => &["bug", "issue", "failure"],
        "failed" => &["error", "failure"],
        "fix" => &["bug", "issue", "error", "repair"],
        "issue" => &["bug", "error", "problem", "fix"],
        "login" => &["auth", "signin", "oauth", "credentials"],
        "perf" => &["performance", "slow", "latency", "optimize"],
        "performance" => &["perf", "slow", "latency", "optimize"],
        "release" => &["deploy", "rollout", "ship"],
        "sid" => &["session", "id", "sessionid", "cookie", "token"],
        "signin" => &["login", "auth", "oauth", "credentials"],
        "slow" => &["performance", "latency", "timeout", "stall"],
        "sso" => &["signin", "login", "auth", "oauth"],
        "stalled" => &["blocked", "waiting", "timeout"],
        "timeout" => &["slow", "latency", "stalled", "hang"],
        "ui" => &["ux", "interface", "frontend"],
        "ux" => &["ui", "interface", "frontend"],
        _ => &[],
    }
}

fn mission_search_phrase_expansions(token: &str) -> &'static [&'static str] {
    match token {
        "ci" => &["continuous integration"],
        "cd" => &["continuous deployment"],
        "sid" => &["session id"],
        "sso" => &["single sign on"],
        _ => &[],
    }
}

fn expand_search_query_group(token: &str) -> Vec<String> {
    let normalized = normalize_metadata_text(token);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut values = HashSet::new();
    values.insert(normalized.clone());
    for candidate in mission_search_synonyms(&normalized) {
        let normalized_candidate = normalize_metadata_text(candidate);
        if !normalized_candidate.is_empty() {
            values.insert(normalized_candidate);
        }
    }

    values.into_iter().collect()
}

fn search_token_match_strength(token: &str, candidate: &str) -> f64 {
    if token == candidate {
        return 1.0;
    }

    let ascii_candidate = candidate
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
    let candidate_len = candidate.chars().count();
    let token_len = token.chars().count();

    if token.starts_with(candidate) && (!ascii_candidate || candidate_len >= 3) {
        return 0.7;
    }
    if ascii_candidate
        && token_len >= 5
        && candidate.starts_with(token)
        && candidate_len.saturating_sub(token_len) <= 2
    {
        return 0.65;
    }
    if candidate_len >= 4 && token.contains(candidate) {
        return 0.45;
    }
    0.0
}

fn search_token_set(text: &str) -> HashSet<String> {
    normalize_metadata_text(text)
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn is_search_stopword(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "at"
            | "did"
            | "do"
            | "does"
            | "for"
            | "from"
            | "how"
            | "i"
            | "in"
            | "is"
            | "it"
            | "me"
            | "my"
            | "of"
            | "on"
            | "or"
            | "our"
            | "please"
            | "show"
            | "that"
            | "the"
            | "this"
            | "to"
            | "us"
            | "was"
            | "we"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "why"
            | "with"
            | "you"
            | "your"
    )
}

#[derive(Debug, Clone)]
struct SearchQueryTerms {
    normalized_query: String,
    normalized_core_query: String,
    query_groups: Vec<Vec<String>>,
    phrase_queries: Vec<String>,
}

fn build_search_query_terms(search_query: &str) -> Option<SearchQueryTerms> {
    let normalized_query = normalize_metadata_text(search_query);
    if normalized_query.is_empty() {
        return None;
    }

    let query_tokens: Vec<&str> = normalized_query.split_whitespace().collect();
    if query_tokens.is_empty() {
        return None;
    }

    let mut filtered_tokens: Vec<&str> = query_tokens
        .iter()
        .copied()
        .filter(|token| !is_search_stopword(token))
        .collect();
    if filtered_tokens.is_empty() {
        filtered_tokens = query_tokens.clone();
    }
    let normalized_core_query = filtered_tokens.join(" ");

    let query_groups: Vec<Vec<String>> = filtered_tokens
        .iter()
        .map(|token| expand_search_query_group(token))
        .filter(|group| !group.is_empty())
        .collect();
    if query_groups.is_empty() {
        return None;
    }

    let mut phrase_queries = Vec::new();
    phrase_queries.push(normalized_core_query.clone());
    for token in &filtered_tokens {
        for phrase in mission_search_phrase_expansions(token) {
            let normalized_phrase = normalize_metadata_text(phrase);
            if !normalized_phrase.is_empty() {
                phrase_queries.push(normalized_phrase);
            }
        }
    }
    phrase_queries.sort();
    phrase_queries.dedup();

    Some(SearchQueryTerms {
        normalized_query,
        normalized_core_query,
        query_groups,
        phrase_queries,
    })
}

fn group_match_strength_for_token_set(group: &[String], token_set: &HashSet<String>) -> f64 {
    let mut best = 0.0;
    for candidate in group {
        if candidate.is_empty() {
            continue;
        }
        for token in token_set {
            let strength = search_token_match_strength(token, candidate);
            if strength > best {
                best = strength;
            }
            if best >= 1.0 {
                return best;
            }
        }
    }
    best
}

fn mission_search_relevance_score(
    mission: &Mission,
    search_query: &str,
    workspace_label: Option<&str>,
) -> f64 {
    let Some(query_terms) = build_search_query_terms(search_query) else {
        return 0.0;
    };
    let phrase_queries = if query_terms.phrase_queries.is_empty() {
        vec![if query_terms.normalized_core_query.is_empty() {
            query_terms.normalized_query.clone()
        } else {
            query_terms.normalized_core_query.clone()
        }]
    } else {
        query_terms.phrase_queries.clone()
    };

    let title = mission.title.as_deref().unwrap_or("").trim();
    let short_description = mission.short_description.as_deref().unwrap_or("").trim();
    let backend = mission.backend.trim();
    let status = mission.status.to_string();
    let display_name = workspace_label.unwrap_or("");
    let combined = format!(
        "{} {} {} {} {}",
        display_name, title, short_description, backend, status
    );
    let normalized_combined = normalize_metadata_text(&combined);
    if normalized_combined.is_empty() {
        return 0.0;
    }

    let fields = [
        (5.0, search_token_set(display_name)),
        (8.0, search_token_set(title)),
        (7.0, search_token_set(short_description)),
        (3.0, search_token_set(backend)),
        (2.0, search_token_set(&status)),
        (1.0, search_token_set(&combined)),
    ];

    let mut score = 0.0;
    for group in &query_terms.query_groups {
        let mut best_group_score: f64 = 0.0;
        for (weight, token_set) in &fields {
            let strength = group_match_strength_for_token_set(group, token_set);
            if strength > 0.0 {
                best_group_score = best_group_score.max(strength * weight);
            }
        }
        if best_group_score <= 0.0 {
            return 0.0;
        }
        score += best_group_score;
    }

    let phrase_boost_targets = [
        (normalize_metadata_text(title), 14.0),
        (normalize_metadata_text(short_description), 12.0),
        (normalize_metadata_text(display_name), 8.0),
        (normalize_metadata_text(&combined), 5.0),
    ];
    for (target, boost) in phrase_boost_targets {
        if target.is_empty() {
            continue;
        }
        if phrase_queries
            .iter()
            .any(|phrase_query| !phrase_query.is_empty() && target.contains(phrase_query))
        {
            score += boost;
        }
    }

    score
}

#[derive(Debug, Clone)]
struct MissionMomentMatch {
    entry_index: usize,
    role: String,
    snippet: String,
    rationale: String,
    relevance_score: f64,
}

fn mission_moment_snippet(content: &str, max_chars: usize) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut result = String::new();
    for ch in collapsed.chars().take(max_chars) {
        result.push(ch);
    }
    result.push('…');
    result
}

fn mission_moment_relevance_score(role: &str, content: &str, search_query: &str) -> f64 {
    let Some(query_terms) = build_search_query_terms(search_query) else {
        return 0.0;
    };
    let phrase_queries = if query_terms.phrase_queries.is_empty() {
        vec![if query_terms.normalized_core_query.is_empty() {
            query_terms.normalized_query.clone()
        } else {
            query_terms.normalized_core_query.clone()
        }]
    } else {
        query_terms.phrase_queries.clone()
    };
    let normalized_content = normalize_metadata_text(content);
    if normalized_content.is_empty() {
        return 0.0;
    }

    let content_tokens = search_token_set(content);
    let role_tokens = search_token_set(role);
    let mut score = 0.0;
    for group in &query_terms.query_groups {
        let content_strength = group_match_strength_for_token_set(group, &content_tokens);
        let role_strength = group_match_strength_for_token_set(group, &role_tokens);
        let best = (content_strength * 8.0).max(role_strength * 1.5);
        if best <= 0.0 {
            return 0.0;
        }
        score += best;
    }

    if phrase_queries
        .iter()
        .any(|phrase_query| !phrase_query.is_empty() && normalized_content.contains(phrase_query))
    {
        score += 10.0;
    }
    score
}

fn mission_moment_rationale(role: &str, content: &str, search_query: &str) -> String {
    let query_terms = match build_search_query_terms(search_query) {
        Some(query_terms) => query_terms,
        None => return format!("Keyword match in {} message", role),
    };

    let phrase_queries = if query_terms.phrase_queries.is_empty() {
        vec![if query_terms.normalized_core_query.is_empty() {
            query_terms.normalized_query.clone()
        } else {
            query_terms.normalized_core_query.clone()
        }]
    } else {
        query_terms.phrase_queries.clone()
    };

    let normalized_content = normalize_metadata_text(content);
    if phrase_queries
        .iter()
        .any(|phrase_query| !phrase_query.is_empty() && normalized_content.contains(phrase_query))
    {
        return format!("Phrase match in {} message", role);
    }

    let content_tokens = search_token_set(content);
    let mut matched_terms = Vec::new();
    for group in &query_terms.query_groups {
        let mut best_candidate: Option<(&str, f64)> = None;
        for candidate in group {
            for token in &content_tokens {
                let strength = search_token_match_strength(token, candidate)
                    .max(search_token_match_strength(candidate, token));
                if strength <= 0.0 {
                    continue;
                }
                match best_candidate {
                    Some((_, best_strength)) if best_strength >= strength => {}
                    _ => best_candidate = Some((candidate.as_str(), strength)),
                }
            }
        }
        if let Some((candidate, _)) = best_candidate {
            matched_terms.push(candidate.to_string());
        }
    }

    if !matched_terms.is_empty() {
        matched_terms.sort();
        matched_terms.dedup();
        let matched_summary = matched_terms
            .into_iter()
            .take(3)
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Matched {} in {} message", matched_summary, role);
    }

    format!("Keyword match in {} message", role)
}

fn best_mission_moment(mission: &Mission, search_query: &str) -> Option<MissionMomentMatch> {
    let mut best: Option<MissionMomentMatch> = None;
    for (idx, entry) in mission.history.iter().enumerate() {
        let score = mission_moment_relevance_score(&entry.role, &entry.content, search_query);
        if score <= 0.0 {
            continue;
        }
        let candidate = MissionMomentMatch {
            entry_index: idx,
            role: entry.role.clone(),
            snippet: mission_moment_snippet(&entry.content, 180),
            rationale: mission_moment_rationale(&entry.role, &entry.content, search_query),
            relevance_score: score,
        };
        match &best {
            Some(existing) if existing.relevance_score >= candidate.relevance_score => {}
            _ => best = Some(candidate),
        }
    }
    best
}

async fn disambiguate_generated_title(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    title: &str,
) -> String {
    const TITLE_DEDUPE_PAGE_SIZE: usize = 200;
    const TITLE_DEDUPE_MAX_SCAN: usize = 5_000;

    let base_title = title.trim();
    if base_title.is_empty() {
        return title.to_string();
    }

    let mut exact_titles = HashSet::new();
    let mut canonical_titles = HashSet::new();
    let mut raw_titles = HashSet::new();
    let mut canonical_raw_titles = HashSet::new();
    let mut dedupe_probe_titles = Vec::new();

    let mut offset = 0;
    while offset < TITLE_DEDUPE_MAX_SCAN {
        let missions = match mission_store
            .list_missions(TITLE_DEDUPE_PAGE_SIZE, offset)
            .await
        {
            Ok(missions) => missions,
            Err(err) => {
                tracing::warn!("Failed to load mission titles for dedupe guard: {}", err);
                return base_title.to_string();
            }
        };

        if missions.is_empty() {
            break;
        }

        for mission in &missions {
            if mission.id == mission_id {
                continue;
            }
            if let Some(existing_title) = &mission.title {
                let raw = normalize_raw_title_for_dedupe(existing_title);
                if !raw.is_empty() {
                    raw_titles.insert(raw);
                }
                let canonical_raw =
                    normalize_raw_title_for_dedupe(strip_numeric_title_suffix(existing_title));
                if !canonical_raw.is_empty() {
                    canonical_raw_titles.insert(canonical_raw);
                }

                let normalized = normalize_metadata_text(existing_title);
                if !normalized.is_empty() {
                    exact_titles.insert(normalized);
                }
                let canonical = canonical_title_key(existing_title);
                if !canonical.is_empty() {
                    canonical_titles.insert(canonical);
                }
                dedupe_probe_titles.push(existing_title.clone());
            }
        }

        if missions.len() < TITLE_DEDUPE_PAGE_SIZE {
            break;
        }
        offset += TITLE_DEDUPE_PAGE_SIZE;
    }

    if offset >= TITLE_DEDUPE_MAX_SCAN {
        tracing::warn!(
            "Title dedupe scan hit cap ({} missions); duplicates may still exist",
            TITLE_DEDUPE_MAX_SCAN
        );
    }

    let candidate_exact = normalize_metadata_text(base_title);
    let candidate_raw = normalize_raw_title_for_dedupe(base_title);
    let candidate_canonical = canonical_title_key(base_title);
    let candidate_canonical_raw =
        normalize_raw_title_for_dedupe(strip_numeric_title_suffix(base_title));

    let is_taken = |candidate: &str| {
        let normalized = normalize_metadata_text(candidate);
        let raw = normalize_raw_title_for_dedupe(candidate);
        let normalized_match = !normalized.is_empty() && exact_titles.contains(&normalized);
        let raw_match = !raw.is_empty() && raw_titles.contains(&raw);
        normalized_match || raw_match
    };

    let has_duplicate = if candidate_exact.is_empty() {
        if candidate_raw.is_empty() {
            false
        } else if candidate_canonical_raw.is_empty() {
            raw_titles.contains(&candidate_raw)
        } else {
            raw_titles.contains(&candidate_raw)
                || canonical_raw_titles.contains(&candidate_canonical_raw)
        }
    } else if candidate_canonical.is_empty() {
        exact_titles.contains(&candidate_exact)
    } else {
        exact_titles.contains(&candidate_exact) || canonical_titles.contains(&candidate_canonical)
    };
    let has_near_duplicate = dedupe_probe_titles
        .iter()
        .any(|existing| is_near_duplicate_title(base_title, existing));
    if !has_duplicate && !has_near_duplicate {
        return base_title.to_string();
    }

    for idx in 2..100 {
        let candidate = format!("{} ({})", base_title, idx);
        if !is_taken(&candidate) {
            return candidate;
        }
    }

    format!("{} ({})", base_title, Uuid::new_v4().as_simple())
}

async fn generate_mission_metadata_updates(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    mission: &Mission,
    history: &[(String, String)],
    fallback_user_content: Option<&str>,
    should_refresh: bool,
) -> (Option<String>, Option<String>) {
    if history.is_empty() {
        return (None, None);
    }

    let title_missing = mission
        .title
        .as_ref()
        .map(|t| t.trim().is_empty())
        .unwrap_or(true);
    let short_description_missing = mission
        .short_description
        .as_ref()
        .map(|d| d.trim().is_empty())
        .unwrap_or(true);
    let title_user_managed =
        mission.metadata_source.as_deref() == Some(METADATA_SOURCE_USER) && !title_missing;
    let has_assistant_reply = history.iter().any(|(role, _)| role == "assistant");
    let has_successful_assistant_reply = history
        .iter()
        .any(|(role, content)| role == "assistant" && assistant_reply_is_successful(content));
    let should_bootstrap_title_from_first_assistant =
        title_missing && has_successful_assistant_reply;
    let should_bootstrap_short_description_from_first_assistant = has_successful_assistant_reply
        && (short_description_missing || title_missing)
        && !should_refresh;

    if !title_missing
        && !short_description_missing
        && !should_refresh
        && !should_bootstrap_title_from_first_assistant
        && !should_bootstrap_short_description_from_first_assistant
    {
        return (None, None);
    }

    // ── Try LLM-powered summarization first ────────────────────────────
    let needs_title = (title_missing || should_refresh) && !title_user_managed;
    let needs_description = short_description_missing
        || should_refresh
        || should_bootstrap_short_description_from_first_assistant;

    if (needs_title || needs_description) && has_successful_assistant_reply {
        if let Some((llm_title, llm_status)) =
            try_llm_metadata_summarization(history, mission, should_refresh).await
        {
            let mut updated_title: Option<String> = None;
            let mut updated_short_description: Option<String> = None;

            if needs_title {
                if let Some(candidate) = llm_title {
                    let candidate = diversify_generated_title_with_recent_context(
                        mission_store,
                        mission_id,
                        candidate,
                        history,
                        fallback_user_content,
                    )
                    .await;
                    if should_accept_metadata_candidate(
                        mission.title.as_deref(),
                        &candidate,
                        should_bootstrap_title_from_first_assistant,
                    ) {
                        updated_title = Some(candidate);
                    }
                }
            }

            if needs_description {
                if let Some(candidate) = llm_status {
                    if should_accept_metadata_candidate(
                        mission.short_description.as_deref(),
                        &candidate,
                        should_bootstrap_short_description_from_first_assistant,
                    ) {
                        updated_short_description = Some(candidate);
                    }
                }
            }

            if updated_title.is_some() || updated_short_description.is_some() {
                return (updated_title, updated_short_description);
            }
        }
    }

    // ── Fallback: heuristic extraction from conversation text ──────────
    let title_candidate = if needs_title {
        let assistant_title_candidate = if should_bootstrap_title_from_first_assistant {
            history
                .iter()
                .filter(|(role, _)| role == "assistant")
                .find_map(|(_, content)| extract_title_from_assistant(content))
        } else {
            history
                .iter()
                .rev()
                .filter(|(role, _)| role == "assistant")
                .find_map(|(_, content)| extract_title_from_assistant(content))
        };

        assistant_title_candidate.or_else(|| {
            if should_bootstrap_title_from_first_assistant {
                fallback_user_content.map(|user_content| {
                    if user_content.len() > 100 {
                        let safe_end = safe_truncate_index(user_content, 100);
                        format!("{}...", &user_content[..safe_end])
                    } else {
                        user_content.to_string()
                    }
                })
            } else {
                None
            }
        })
    } else {
        None
    };
    let title_candidate = match title_candidate {
        Some(candidate) => Some(
            diversify_generated_title_with_recent_context(
                mission_store,
                mission_id,
                candidate,
                history,
                fallback_user_content,
            )
            .await,
        ),
        None => None,
    };

    let short_description_candidate = if needs_description {
        if should_bootstrap_short_description_from_first_assistant {
            extract_short_description_from_first_successful_assistant(history, 160)
                .or_else(|| extract_short_description_from_history(history, 160))
        } else if should_refresh {
            extract_short_description_from_recent_history(history, 160)
                .or_else(|| extract_short_description_from_history(history, 160))
        } else if has_assistant_reply {
            None
        } else {
            extract_short_description_from_history(history, 160)
        }
    } else {
        None
    };

    let mut updated_title: Option<String> = None;
    if let Some(candidate_title) = title_candidate {
        if should_accept_metadata_candidate(
            mission.title.as_deref(),
            &candidate_title,
            should_bootstrap_title_from_first_assistant,
        ) {
            updated_title = Some(candidate_title);
        }
    }

    let mut updated_short_description: Option<String> = None;
    if let Some(candidate_short_description) = short_description_candidate {
        if should_accept_metadata_candidate(
            mission.short_description.as_deref(),
            &candidate_short_description,
            should_bootstrap_short_description_from_first_assistant,
        ) {
            updated_short_description = Some(candidate_short_description);
        }
    }

    (updated_title, updated_short_description)
}

/// Check if a metadata candidate should replace the existing value.
fn should_accept_metadata_candidate(
    existing: Option<&str>,
    candidate: &str,
    is_bootstrap: bool,
) -> bool {
    let should_update = if is_bootstrap {
        true
    } else {
        existing
            .map(|e| has_significant_metadata_drift(e, candidate))
            .unwrap_or(true)
    };
    if !should_update {
        return false;
    }
    existing
        .map(|e| normalize_metadata_text(e) != normalize_metadata_text(candidate))
        .unwrap_or(true)
}

/// Try to generate metadata using the configured LLM provider.
/// Returns `Some((title, status))` if the LLM was called successfully,
/// `None` if no LLM is configured or the call failed.
async fn try_llm_metadata_summarization(
    history: &[(String, String)],
    mission: &Mission,
    is_refresh: bool,
) -> Option<(Option<String>, Option<String>)> {
    let llm = match super::metadata_llm::metadata_llm() {
        Some(l) => l,
        None => {
            tracing::debug!("[MetadataLLM] No LLM client available");
            return None;
        }
    };

    // Gather the most relevant user message and assistant reply
    let (user_msg, assistant_msg) = if is_refresh {
        // On refresh, use the most recent exchange
        let user = history
            .iter()
            .rev()
            .find(|(role, _)| role == "user")
            .map(|(_, c)| c.as_str())
            .unwrap_or("");
        let assistant = history
            .iter()
            .rev()
            .find(|(role, content)| role == "assistant" && assistant_reply_is_successful(content))
            .map(|(_, c)| c.as_str())
            .unwrap_or("");
        (user, assistant)
    } else {
        // On bootstrap, use the first exchange
        let user = history
            .iter()
            .find(|(role, _)| role == "user")
            .map(|(_, c)| c.as_str())
            .unwrap_or("");
        let assistant = history
            .iter()
            .find(|(role, content)| role == "assistant" && assistant_reply_is_successful(content))
            .map(|(_, c)| c.as_str())
            .unwrap_or("");
        (user, assistant)
    };

    if user_msg.is_empty() && assistant_msg.is_empty() {
        return None;
    }

    tracing::info!(
        "[MetadataLLM] Calling summarize_mission (is_refresh={}, user_len={}, assistant_len={})",
        is_refresh,
        user_msg.len(),
        assistant_msg.len()
    );

    let (title, status) = llm
        .summarize_mission(
            user_msg,
            assistant_msg,
            mission.title.as_deref(),
            is_refresh,
        )
        .await;

    tracing::info!(
        "[MetadataLLM] Result: title={:?}, status={:?}",
        title,
        status
    );

    // Only return Some if we got at least one result
    if title.is_some() || status.is_some() {
        Some((title, status))
    } else {
        None
    }
}

async fn apply_generated_mission_metadata_updates(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    generated_title: Option<String>,
    generated_short_description: Option<String>,
    metadata_source_update: Option<Option<&str>>,
    metadata_model: Option<&str>,
) -> bool {
    if generated_title.is_none() && generated_short_description.is_none() {
        return false;
    }

    let _title_update_guard = if generated_title.is_some() {
        Some(MISSION_TITLE_UPDATE_LOCK.lock().await)
    } else {
        None
    };

    let title_to_write = match generated_title {
        Some(title) => Some(disambiguate_generated_title(mission_store, mission_id, &title).await),
        None => None,
    };

    if let Err(err) = mission_store
        .update_mission_metadata(
            mission_id,
            title_to_write.as_deref().map(Some),
            generated_short_description.as_deref().map(Some),
            metadata_source_update,
            metadata_model.map(Some),
            Some(Some(METADATA_VERSION_V1)),
        )
        .await
    {
        tracing::warn!("Failed to update mission metadata: {}", err);
        return false;
    }

    match mission_store.get_mission(mission_id).await {
        Ok(Some(updated)) => {
            emit_mission_metadata_updated_event(events_tx, mission_id, &updated);
            if title_to_write.is_some() {
                if let Some(title) = updated.title {
                    let _ = events_tx.send(AgentEvent::MissionTitleChanged { mission_id, title });
                }
            }
        }
        Ok(None) => {
            tracing::warn!("Mission {} disappeared after metadata update", mission_id);
        }
        Err(err) => {
            tracing::warn!("Failed to reload mission metadata after update: {}", err);
        }
    }
    true
}

fn emit_mission_metadata_updated_event(
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    mission: &Mission,
) {
    let _ = events_tx.send(AgentEvent::MissionMetadataUpdated {
        mission_id,
        title: mission.title.clone(),
        short_description: mission.short_description.clone(),
        metadata_updated_at: mission.metadata_updated_at.clone(),
        updated_at: Some(mission.updated_at.clone()),
        metadata_source: mission.metadata_source.clone(),
        metadata_model: mission.metadata_model.clone(),
        metadata_version: mission.metadata_version.clone(),
    });
}

fn conversational_message_count(history: &[(String, String)]) -> usize {
    history
        .iter()
        .filter(|(role, _)| role == "user" || role == "assistant")
        .count()
}

fn should_refresh_metadata_by_cadence(
    mission_id: Uuid,
    mission: &Mission,
    conversational_count: usize,
    force_refresh: bool,
) -> bool {
    if force_refresh {
        return true;
    }
    if conversational_count == 0 {
        return false;
    }

    let mut baselines = MISSION_METADATA_REFRESH_BASELINES
        .lock()
        .expect("metadata refresh baseline lock poisoned");
    let baseline = baselines.entry(mission_id).or_insert_with(|| {
        if mission.metadata_updated_at.is_some() {
            conversational_count
        } else {
            0
        }
    });

    if conversational_count < *baseline {
        *baseline = conversational_count;
        return false;
    }

    conversational_count.saturating_sub(*baseline) >= 10
}

fn record_metadata_refresh_baseline(mission_id: Uuid, conversational_count: usize) {
    let mut baselines = MISSION_METADATA_REFRESH_BASELINES
        .lock()
        .expect("metadata refresh baseline lock poisoned");
    baselines.insert(mission_id, conversational_count);
}

fn record_metadata_refresh_baseline_from_mission(mission_id: Uuid, mission: &Mission) {
    let conversational_count = mission
        .history
        .iter()
        .filter(|entry| entry.role == "user" || entry.role == "assistant")
        .count();
    record_metadata_refresh_baseline(mission_id, conversational_count);
}

async fn persist_mission_history_and_schedule_metadata_refresh(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    entries: &[MissionHistoryEntry],
) {
    if let Err(e) = mission_store
        .update_mission_history(mission_id, entries)
        .await
    {
        tracing::warn!("Failed to persist mission history: {}", e);
        return;
    }
    schedule_mission_metadata_refresh(mission_store, events_tx, mission_id, false);
}

async fn refresh_mission_metadata_from_store(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    force_refresh: bool,
) {
    let mission = match mission_store.get_mission(mission_id).await {
        Ok(Some(mission)) => mission,
        Ok(None) => {
            tracing::warn!(
                "Skipping metadata refresh; mission {} not found",
                mission_id
            );
            return;
        }
        Err(err) => {
            tracing::warn!(
                "Failed to load mission {} for metadata refresh: {}",
                mission_id,
                err
            );
            return;
        }
    };

    let history_pairs: Vec<(String, String)> = mission
        .history
        .iter()
        .map(|entry| (entry.role.clone(), entry.content.clone()))
        .collect();
    let fallback_user_content = history_pairs
        .iter()
        .find(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str());
    let conversational_count = conversational_message_count(&history_pairs);
    let should_refresh = should_refresh_metadata_by_cadence(
        mission_id,
        &mission,
        conversational_count,
        force_refresh,
    );

    let (generated_title, generated_short_description) = generate_mission_metadata_updates(
        mission_store,
        mission_id,
        &mission,
        &history_pairs,
        fallback_user_content,
        should_refresh,
    )
    .await;
    let metadata_source_update = if generated_title.is_none()
        && mission.metadata_source.as_deref() == Some(METADATA_SOURCE_USER)
        && mission
            .title
            .as_deref()
            .map(|title| !title.trim().is_empty())
            .unwrap_or(false)
    {
        None
    } else {
        Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC))
    };
    let metadata_updated = apply_generated_mission_metadata_updates(
        mission_store,
        events_tx,
        mission_id,
        generated_title,
        generated_short_description,
        metadata_source_update,
        mission.model_override.as_deref(),
    )
    .await;
    if should_refresh || metadata_updated {
        record_metadata_refresh_baseline(mission_id, conversational_count);
    }
}

fn schedule_mission_metadata_refresh(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    force_refresh: bool,
) {
    {
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        if should_skip_metadata_refresh_schedule(&mut tasks, mission_id, force_refresh) {
            return;
        }
    }

    let task_id = MISSION_METADATA_REFRESH_TASK_ID.fetch_add(1, Ordering::Relaxed);
    let mission_store = Arc::clone(mission_store);
    let events_tx = events_tx.clone();
    let background_refresh = tokio::spawn(async move {
        refresh_mission_metadata_from_store(&mission_store, &events_tx, mission_id, force_refresh)
            .await;
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        complete_metadata_refresh_task(&mut tasks, mission_id, task_id);
    });
    let registration = {
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        register_metadata_refresh_task(
            &mut tasks,
            mission_id,
            force_refresh,
            task_id,
            background_refresh,
        )
    };
    if let Some(superseded) = registration.superseded {
        superseded.abort();
    }
}

async fn refresh_mission_metadata_for_milestone(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
) {
    refresh_mission_metadata_from_store(mission_store, events_tx, mission_id, true).await;
}

fn schedule_mission_metadata_refresh_for_milestone(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
) {
    {
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        if should_skip_metadata_refresh_schedule(&mut tasks, mission_id, true) {
            return;
        }
    }

    let task_id = MISSION_METADATA_REFRESH_TASK_ID.fetch_add(1, Ordering::Relaxed);
    let mission_store = Arc::clone(mission_store);
    let events_tx = events_tx.clone();
    let background_refresh = tokio::spawn(async move {
        refresh_mission_metadata_for_milestone(&mission_store, &events_tx, mission_id).await;
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        complete_metadata_refresh_task(&mut tasks, mission_id, task_id);
    });
    let registration = {
        let mut tasks = MISSION_METADATA_REFRESH_TASKS
            .lock()
            .expect("metadata refresh task registry lock poisoned");
        register_metadata_refresh_task(&mut tasks, mission_id, true, task_id, background_refresh)
    };
    if let Some(superseded) = registration.superseded {
        superseded.abort();
    }
}

fn status_requires_metadata_milestone_refresh(status: MissionStatus) -> bool {
    !matches!(status, MissionStatus::Pending | MissionStatus::Active)
}

fn maybe_schedule_mission_metadata_refresh_for_status(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    status: MissionStatus,
) {
    if status_requires_metadata_milestone_refresh(status) {
        schedule_mission_metadata_refresh_for_milestone(mission_store, events_tx, mission_id);
    }
}

/// Error returned when the control session command channel is closed.
fn session_unavailable<T>(_: T) -> (StatusCode, String) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "control session unavailable".to_string(),
    )
}

/// Error returned when a oneshot response channel is dropped.
fn recv_failed<T>(_: T) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to receive response".to_string(),
    )
}

/// Shorthand for a `{ "ok": true }` JSON response.
fn ok_json() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

/// Unwrap a mission ID or emit an error event and return a failure result.
#[allow(clippy::result_large_err)]
fn require_mission_id(
    mission_id: Option<Uuid>,
    backend: &str,
    events_tx: &broadcast::Sender<AgentEvent>,
) -> Result<Uuid, crate::agents::AgentResult> {
    mission_id.ok_or_else(|| {
        let msg = format!("{} backend requires a mission ID", backend);
        let _ = events_tx.send(AgentEvent::Error {
            message: msg.clone(),
            mission_id: None,
            resumable: false,
        });
        crate::agents::AgentResult::failure(msg, 0).with_terminal_reason(TerminalReason::LlmError)
    })
}

/// Query the control actor for the list of currently running missions.
async fn get_running_missions(
    control: &ControlState,
) -> Result<Vec<super::mission_runner::RunningMissionInfo>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::ListRunning { respond: tx })
        .await
        .map_err(session_unavailable)?;
    rx.await.map_err(recv_failed)
}

/// Look up an automation by ID, returning 404 if it does not exist.
async fn require_automation(
    store: &Arc<dyn MissionStore>,
    id: Uuid,
) -> Result<mission_store::Automation, (StatusCode, String)> {
    store
        .get_automation(id)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("Automation {} not found", id),
        ))
}

/// Validate that a command exists in the library.
async fn validate_library_command(
    state: &AppState,
    name: &str,
) -> Result<(), (StatusCode, String)> {
    if let Some(lib) = state.library.read().await.as_ref() {
        match lib.get_command(name).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not found") || msg.contains("does not exist") {
                    Err((
                        StatusCode::BAD_REQUEST,
                        format!("Command '{}' not found in library", name),
                    ))
                } else {
                    Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to validate command: {}", e),
                    ))
                }
            }
        }
    } else {
        Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Library not initialized".to_string(),
        ))
    }
}

async fn mission_has_active_automation(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
) -> bool {
    match mission_store.get_mission_automations(mission_id).await {
        Ok(automations) => automations.iter().any(|automation| automation.active),
        Err(err) => {
            tracing::warn!(
                "Failed to load automations for mission {}: {}",
                mission_id,
                err
            );
            false
        }
    }
}

async fn stop_policy_matches_status(
    stop_policy: &mission_store::StopPolicy,
    _status: MissionStatus,
    consecutive_failures: u32,
) -> bool {
    match stop_policy {
        mission_store::StopPolicy::Never => false,
        mission_store::StopPolicy::WhenFailingConsecutively { count } => {
            consecutive_failures >= *count
        }
        mission_store::StopPolicy::WhenAllIssuesClosedAndPRsMerged { repo } => {
            check_github_all_issues_closed_and_prs_merged(repo).await
        }
    }
}

async fn consecutive_failure_count_for_automation(
    mission_store: &Arc<dyn MissionStore>,
    automation: &mission_store::Automation,
) -> u32 {
    if !matches!(
        automation.stop_policy,
        mission_store::StopPolicy::WhenFailingConsecutively { .. }
    ) {
        return 0;
    }

    let executions = mission_store
        .get_automation_executions(automation.id, Some(20))
        .await
        .unwrap_or_default();
    let mut count = 0u32;
    for exec in executions.iter().take(20) {
        match exec.status {
            mission_store::ExecutionStatus::Failed => count += 1,
            mission_store::ExecutionStatus::Success => break,
            _ => {}
        }
    }
    count
}

async fn check_github_all_issues_closed_and_prs_merged(repo: &str) -> bool {
    let client = reqwest::Client::new();

    let issues_url = format!("https://api.github.com/repos/{}/issues?state=open", repo);
    let pulls_url = format!("https://api.github.com/repos/{}/pulls?state=open", repo);

    let check_repo = async {
        let issues_resp = client
            .get(&issues_url)
            .header("User-Agent", "sandboxed-sh")
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await;

        let pulls_resp = client
            .get(&pulls_url)
            .header("User-Agent", "sandboxed-sh")
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await;

        let has_open_issues = match issues_resp {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(value) => value.as_array().map(|arr| !arr.is_empty()).unwrap_or(false),
                Err(_) => false,
            },
            Err(_) => false,
        };

        let has_open_prs = match pulls_resp {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(value) => value.as_array().map(|arr| !arr.is_empty()).unwrap_or(false),
                Err(_) => false,
            },
            Err(_) => false,
        };

        !has_open_issues && !has_open_prs
    };

    tokio::time::timeout(std::time::Duration::from_secs(10), check_repo)
        .await
        .unwrap_or(false)
}

pub(crate) async fn resolve_claudecode_default_model(
    library: &SharedLibrary,
    config_profile: Option<&str>,
) -> Option<String> {
    let lib = {
        let guard = library.read().await;
        guard.clone()
    }?;

    let profile = config_profile.unwrap_or("default");
    match lib.get_claudecode_config_for_profile(profile).await {
        Ok(config) => config.default_model.and_then(|model| {
            let trimmed = model.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }),
        Err(err) => {
            tracing::warn!(
                "Failed to load Claude Code config from library (profile: {}): {}",
                profile,
                err
            );
            None
        }
    }
}

/// Return the default model for Gemini CLI when no override is specified.
pub(crate) fn resolve_gemini_default_model() -> String {
    "gemini-3.1-pro-preview".to_string()
}

async fn close_mission_desktop_sessions(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    working_dir: &std::path::Path,
) {
    let Ok(Some(mission)) = mission_store.get_mission(mission_id).await else {
        return;
    };

    if mission.desktop_sessions.is_empty() {
        return;
    }

    let mut sessions = mission.desktop_sessions.clone();
    let now = now_string();
    let mut updated = false;

    for session in sessions
        .iter_mut()
        .filter(|session| session.stopped_at.is_none())
    {
        if let Err(err) = desktop::close_desktop_session(&session.display, working_dir).await {
            tracing::warn!(
                mission_id = %mission_id,
                display = %session.display,
                error = %err,
                "Failed to close desktop session"
            );
        }
        session.stopped_at = Some(now.clone());
        updated = true;
    }

    if updated {
        if let Err(err) = mission_store
            .update_mission_desktop_sessions(mission_id, &sessions)
            .await
        {
            tracing::warn!(
                mission_id = %mission_id,
                error = %err,
                "Failed to persist desktop session shutdown"
            );
        }
    }
}

/// Message posted by a user to the control session.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlMessageRequest {
    pub content: String,
    /// Optional agent override for this specific message (e.g., from @agent mention)
    #[serde(default)]
    pub agent: Option<String>,
    /// Target mission ID. If provided and differs from the currently running mission,
    /// the backend will automatically start this mission in parallel (if capacity allows).
    #[serde(default)]
    pub mission_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlMessageResponse {
    pub id: Uuid,
    pub queued: bool,
}

/// A message waiting in the queue
#[derive(Debug, Clone, Serialize)]
pub struct QueuedMessage {
    pub id: Uuid,
    pub content: String,
    pub agent: Option<String>,
    /// Which mission this queued message belongs to
    pub mission_id: Option<Uuid>,
}

/// Tool result posted by the frontend for an interactive tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct ControlToolResultRequest {
    pub tool_call_id: String,
    pub name: String,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlRunState {
    #[default]
    Idle,
    Running,
    WaitingForTool,
}

/// A file shared by the agent (images render inline, other files show as download links).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedFile {
    /// Display name for the file
    pub name: String,
    /// Public URL to view/download
    pub url: String,
    /// MIME type (e.g., "image/png", "application/pdf")
    pub content_type: String,
    /// File size in bytes (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// File kind for rendering hints: "image", "document", "archive", "code", "other"
    pub kind: SharedFileKind,
}

/// Kind of shared file (determines how it renders in the UI).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedFileKind {
    /// Images (PNG, JPEG, GIF, WebP, SVG) - rendered inline
    Image,
    /// Documents (PDF, Word, etc.) - shown as download card
    Document,
    /// Archives (ZIP, TAR, etc.) - shown as download card
    Archive,
    /// Code/text files - shown as download card with syntax hint
    Code,
    /// Other files - generic download card
    Other,
}

impl SharedFile {
    /// Create a new SharedFile, inferring kind from content_type.
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        content_type: impl Into<String>,
        size_bytes: Option<u64>,
    ) -> Self {
        let content_type = content_type.into();
        let kind = Self::infer_kind(&content_type);
        Self {
            name: name.into(),
            url: url.into(),
            content_type,
            size_bytes,
            kind,
        }
    }

    /// Infer the file kind from MIME type.
    fn infer_kind(content_type: &str) -> SharedFileKind {
        if content_type.starts_with("image/") {
            SharedFileKind::Image
        } else if content_type.starts_with("text/")
            || content_type.contains("json")
            || content_type.contains("xml")
        {
            SharedFileKind::Code
        } else if content_type.contains("pdf")
            || content_type.contains("document")
            || content_type.contains("word")
        {
            SharedFileKind::Document
        } else if content_type.contains("zip")
            || content_type.contains("tar")
            || content_type.contains("gzip")
            || content_type.contains("compress")
        {
            SharedFileKind::Archive
        } else {
            SharedFileKind::Other
        }
    }
}

// ---------------------------------------------------------------------------
// Rich tag parsing: extract <image path="..." /> and <file path="..." /> from
// agent output so we can validate referenced files and populate shared_files.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum RichTagType {
    Image,
    File,
}

#[derive(Debug, Clone)]
struct RichTagRef {
    tag_type: RichTagType,
    path: String,
    alt: Option<String>,
    name: Option<String>,
}

/// Parse `<image path="..." />` and `<file path="..." />` tags from content.
fn parse_rich_tags(content: &str) -> Vec<RichTagRef> {
    use regex::Regex;
    use std::sync::LazyLock;

    static TAG_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<(image|file)\s+([^>]*?)\s*/>"#).unwrap());
    static ATTR_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(\w+)\s*=\s*"([^"]*)""#).unwrap());

    let mut tags = Vec::new();
    for cap in TAG_RE.captures_iter(content) {
        let tag_type = match cap[1].to_ascii_lowercase().as_str() {
            "image" => RichTagType::Image,
            "file" => RichTagType::File,
            _ => continue,
        };
        let attr_str = &cap[2];
        let mut path = None;
        let mut alt = None;
        let mut name = None;
        for attr_cap in ATTR_RE.captures_iter(attr_str) {
            match &attr_cap[1] {
                "path" => path = Some(attr_cap[2].to_string()),
                "alt" => alt = Some(attr_cap[2].to_string()),
                "name" => name = Some(attr_cap[2].to_string()),
                _ => {}
            }
        }
        if let Some(p) = path {
            tags.push(RichTagRef {
                tag_type,
                path: p,
                alt,
                name,
            });
        }
    }
    tags
}

/// Validate rich tag paths against the filesystem and return SharedFile entries.
/// `working_dir` is used to resolve relative paths.
/// `workspace_id` and `mission_id` are included in download URLs for the frontend.
async fn validate_rich_tags(
    tags: &[RichTagRef],
    working_dir: &std::path::Path,
    workspace_id: Option<Uuid>,
    mission_id: Option<Uuid>,
) -> Vec<SharedFile> {
    // Only allow files that resolve within the mission working directory. This keeps the
    // "shared files" surface area consistent with what the agent produced in its workspace,
    // and avoids emitting links that would be rejected by the download endpoint anyway.
    let canonical_working_dir = working_dir.canonicalize().ok();

    let mut files = Vec::new();
    for tag in tags {
        // Resolve the path relative to working_dir
        let p = std::path::Path::new(&tag.path);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            working_dir.join(&tag.path)
        };

        // Check existence and metadata
        let meta = match tokio::fs::metadata(&resolved).await {
            Ok(m) => m,
            Err(_) => continue, // skip non-existent files
        };

        let canon_resolved = match resolved.canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(work_root) = canonical_working_dir.as_ref() {
            if !canon_resolved.starts_with(work_root) {
                continue;
            }
        }

        let size = Some(meta.len());
        let content_type = super::fs::content_type_for_path(&canon_resolved).to_string();

        let display_name = match &tag.tag_type {
            RichTagType::Image => tag
                .alt
                .clone()
                .or_else(|| tag.path.rsplit('/').next().map(|s| s.to_string()))
                .unwrap_or_else(|| tag.path.clone()),
            RichTagType::File => tag
                .name
                .clone()
                .or_else(|| tag.path.rsplit('/').next().map(|s| s.to_string()))
                .unwrap_or_else(|| tag.path.clone()),
        };

        // Build a download URL for the file
        let canon_str = canon_resolved.to_string_lossy();
        let mut url = format!(
            "/api/fs/download?path={}",
            urlencoding::encode(canon_str.as_ref())
        );
        if let Some(ws_id) = workspace_id {
            url.push_str(&format!("&workspace_id={}", ws_id));
        }
        if let Some(mid) = mission_id {
            url.push_str(&format!("&mission_id={}", mid));
        }

        files.push(SharedFile::new(display_name, url, content_type, size));
    }
    files
}

/// A structured event emitted by the control session.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Status {
        state: ControlRunState,
        queue_len: usize,
        /// Mission this status applies to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    UserMessage {
        id: Uuid,
        content: String,
        /// Whether this message is queued (not yet being processed).
        #[serde(default)]
        queued: bool,
        /// Mission this message belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    AssistantMessage {
        id: Uuid,
        content: String,
        success: bool,
        cost_cents: u64,
        cost_source: crate::agents::CostSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<crate::cost::TokenUsage>,
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_normalized: Option<String>,
        /// Mission this message belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
        /// Files shared in this message (images, documents, etc.)
        #[serde(skip_serializing_if = "Option::is_none")]
        shared_files: Option<Vec<SharedFile>>,
        /// Whether the mission can be resumed after this failure (only relevant when success=false)
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumable: bool,
    },
    /// Agent thinking/reasoning (streaming)
    Thinking {
        /// Incremental thinking content
        content: String,
        /// Whether this is the final thinking chunk
        done: bool,
        /// Mission this thinking belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Text content delta (streaming assistant response)
    TextDelta {
        /// Accumulated text content so far
        content: String,
        /// Mission this text belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    ToolCall {
        tool_call_id: String,
        name: String,
        args: serde_json::Value,
        /// Mission this tool call belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
        /// Mission this result belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    Error {
        message: String,
        /// Mission this error belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
        /// Whether the mission can be resumed after this error
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resumable: bool,
    },
    /// Goal-mode iteration marker — fired once per turn while a codex
    /// `/goal` continuation loop is active. UI renders as "iter N" pill.
    GoalIteration {
        iteration: u32,
        objective: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Goal status transitioned. Carries the canonical status string from
    /// codex's `thread/goal/updated`: `active`, `paused`, `budgetLimited`,
    /// `complete`, or `cleared` when the goal was explicitly aborted.
    GoalStatus {
        status: String,
        objective: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Mission status changed (by agent or user)
    MissionStatusChanged {
        mission_id: Uuid,
        status: MissionStatus,
        summary: Option<String>,
    },
    /// Mission title changed (by user)
    MissionTitleChanged { mission_id: Uuid, title: String },
    /// Mission metadata changed (title/short description refresh)
    MissionMetadataUpdated {
        mission_id: Uuid,
        title: Option<String>,
        short_description: Option<String>,
        metadata_updated_at: Option<String>,
        updated_at: Option<String>,
        metadata_source: Option<String>,
        metadata_model: Option<String>,
        metadata_version: Option<String>,
    },
    /// Mission run settings changed (backend/model/agent/config profile)
    MissionSettingsUpdated {
        mission_id: Uuid,
        backend: String,
        agent: Option<String>,
        model_override: Option<String>,
        model_effort: Option<String>,
        config_profile: Option<String>,
        session_id: Option<String>,
        updated_at: String,
    },
    /// Agent phase update (for showing preparation steps)
    AgentPhase {
        /// Phase name: "executing", "delegating", etc.
        phase: String,
        /// Optional details about what's happening
        detail: Option<String>,
        /// Agent name (for hierarchical display)
        agent: Option<String>,
        /// Mission this phase belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Agent tree update (for real-time tree visualization)
    AgentTree {
        /// The full agent tree structure
        tree: AgentTreeNode,
        /// Mission this tree belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Execution progress update (for progress indicator)
    Progress {
        /// Total number of subtasks
        total_subtasks: usize,
        /// Number of completed subtasks
        completed_subtasks: usize,
        /// Currently executing subtask description (if any)
        current_subtask: Option<String>,
        /// Current depth level (0=root, 1=subtask, 2=sub-subtask)
        depth: u8,
        /// Mission this progress belongs to (for parallel execution)
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// Session ID update (for backends like Amp that generate their own session IDs)
    SessionIdUpdate {
        /// The new session ID to use for continuation
        session_id: String,
        /// Mission this session ID belongs to
        mission_id: Uuid,
    },
    /// Live activity label derived from the current tool call
    MissionActivity {
        /// Human-readable activity label (e.g., "Reading: main.rs")
        label: String,
        /// Tool name that generated this activity
        tool_name: String,
        /// Mission this activity belongs to
        #[serde(skip_serializing_if = "Option::is_none")]
        mission_id: Option<Uuid>,
    },
    /// FIDO signing approval request forwarded to the mobile app
    FidoSignRequest {
        request_id: Uuid,
        key_type: String,
        key_fingerprint: String,
        origin: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        expires_at: String,
    },
}

/// A node in the agent tree (for visualization)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTreeNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String, // e.g. "Root", "Worker"
    pub name: String,
    pub description: String,
    pub status: String, // "pending", "running", "completed", "failed"
    pub budget_allocated: u64,
    pub budget_spent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    #[serde(default)]
    pub children: Vec<AgentTreeNode>,
}

impl AgentTreeNode {
    pub fn new(id: &str, node_type: &str, name: &str, description: &str) -> Self {
        Self {
            id: id.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            status: "pending".to_string(),
            budget_allocated: 0,
            budget_spent: 0,
            complexity: None,
            selected_model: None,
            children: Vec::new(),
        }
    }

    pub fn with_budget(mut self, allocated: u64, spent: u64) -> Self {
        self.budget_allocated = allocated;
        self.budget_spent = spent;
        self
    }

    pub fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub fn with_complexity(mut self, complexity: f64) -> Self {
        self.complexity = Some(complexity);
        self
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.selected_model = Some(model.to_string());
        self
    }

    pub fn add_child(&mut self, child: AgentTreeNode) {
        self.children.push(child);
    }
}

impl AgentEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            AgentEvent::Status { .. } => "status",
            AgentEvent::UserMessage { .. } => "user_message",
            AgentEvent::AssistantMessage { .. } => "assistant_message",
            AgentEvent::Thinking { .. } => "thinking",
            AgentEvent::TextDelta { .. } => "text_delta",
            AgentEvent::ToolCall { .. } => "tool_call",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Error { .. } => "error",
            AgentEvent::MissionStatusChanged { .. } => "mission_status_changed",
            AgentEvent::AgentPhase { .. } => "agent_phase",
            AgentEvent::AgentTree { .. } => "agent_tree",
            AgentEvent::Progress { .. } => "progress",
            AgentEvent::SessionIdUpdate { .. } => "session_id_update",
            AgentEvent::MissionActivity { .. } => "mission_activity",
            AgentEvent::MissionTitleChanged { .. } => "mission_title_changed",
            AgentEvent::MissionMetadataUpdated { .. } => "mission_metadata_updated",
            AgentEvent::MissionSettingsUpdated { .. } => "mission_settings_updated",
            AgentEvent::FidoSignRequest { .. } => "fido_sign_request",
            AgentEvent::GoalIteration { .. } => "goal_iteration",
            AgentEvent::GoalStatus { .. } => "goal_status",
        }
    }

    pub fn mission_id(&self) -> Option<Uuid> {
        match self {
            AgentEvent::Status { mission_id, .. } => *mission_id,
            AgentEvent::UserMessage { mission_id, .. } => *mission_id,
            AgentEvent::AssistantMessage { mission_id, .. } => *mission_id,
            AgentEvent::Thinking { mission_id, .. } => *mission_id,
            AgentEvent::TextDelta { mission_id, .. } => *mission_id,
            AgentEvent::ToolCall { mission_id, .. } => *mission_id,
            AgentEvent::ToolResult { mission_id, .. } => *mission_id,
            AgentEvent::Error { mission_id, .. } => *mission_id,
            AgentEvent::MissionStatusChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
            AgentEvent::AgentTree { mission_id, .. } => *mission_id,
            AgentEvent::Progress { mission_id, .. } => *mission_id,
            AgentEvent::SessionIdUpdate { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionActivity { mission_id, .. } => *mission_id,
            AgentEvent::MissionTitleChanged { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionMetadataUpdated { mission_id, .. } => Some(*mission_id),
            AgentEvent::MissionSettingsUpdated { mission_id, .. } => Some(*mission_id),
            AgentEvent::FidoSignRequest { .. } => None,
            AgentEvent::GoalIteration { mission_id, .. } => *mission_id,
            AgentEvent::GoalStatus { mission_id, .. } => *mission_id,
        }
    }
}

/// Internal control commands (queued and processed by the actor).
#[derive(Debug)]
pub enum ControlCommand {
    UserMessage {
        id: Uuid,
        content: String,
        /// Optional agent override for this specific message
        agent: Option<String>,
        /// Target mission ID - if provided and differs from running mission, start in parallel
        target_mission_id: Option<Uuid>,
        /// Respond with whether the message was queued (true = waiting to be processed)
        respond: oneshot::Sender<bool>,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
    },
    Cancel,
    /// Load a mission (switch to it)
    LoadMission {
        id: Uuid,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Create a new mission
    CreateMission {
        title: Option<String>,
        workspace_id: Option<Uuid>,
        /// Agent name from library (e.g., "code-reviewer")
        agent: Option<String>,
        /// Optional model override (provider/model)
        model_override: Option<String>,
        /// Optional model effort override (e.g. low/medium/high/xhigh/max)
        model_effort: Option<String>,
        /// Backend to use for this mission ("opencode" or "claudecode")
        backend: Option<String>,
        /// Config profile to use for this mission
        config_profile: Option<String>,
        /// Parent mission ID (for orchestrated worker missions)
        parent_mission_id: Option<Uuid>,
        /// Working directory override (for git worktrees etc.)
        working_directory: Option<String>,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Update mission status
    SetMissionStatus {
        id: Uuid,
        status: MissionStatus,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Update mission title
    SetMissionTitle {
        id: Uuid,
        title: String,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Update mission run settings
    UpdateMissionSettings {
        id: Uuid,
        backend: Option<String>,
        agent: Option<Option<String>>,
        model_override: Option<Option<String>>,
        model_effort: Option<Option<String>>,
        config_profile: Option<Option<String>>,
        session_id: String,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Start a mission in parallel (if slots available)
    StartParallel {
        mission_id: Uuid,
        content: String,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// Cancel a specific mission
    CancelMission {
        mission_id: Uuid,
        respond: oneshot::Sender<Result<(), String>>,
    },
    /// List currently running missions
    ListRunning {
        respond: oneshot::Sender<Vec<super::mission_runner::RunningMissionInfo>>,
    },
    /// Resume an interrupted mission
    ResumeMission {
        mission_id: Uuid,
        /// If true, clean the mission's work directory before resuming
        clean_workspace: bool,
        /// If true, only update status without sending the automatic resume message
        skip_message: bool,
        respond: oneshot::Sender<Result<Mission, String>>,
    },
    /// Graceful shutdown - mark running missions as interrupted
    GracefulShutdown {
        respond: oneshot::Sender<Vec<Uuid>>,
    },
    /// Get the current message queue
    GetQueue {
        respond: oneshot::Sender<Vec<QueuedMessage>>,
    },
    /// Remove a message from the queue
    RemoveFromQueue {
        message_id: Uuid,
        respond: oneshot::Sender<bool>, // true if removed, false if not found
    },
    /// Clear all messages from the queue
    ClearQueue {
        respond: oneshot::Sender<usize>, // number of messages cleared
    },
}

// ==================== Mission Types ====================

/// Mission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    /// Mission created but hasn't received any messages yet
    Pending,
    Active,
    Completed,
    Failed,
    /// Mission was interrupted (server shutdown, cancellation, etc.)
    Interrupted,
    /// Mission blocked by external factors (type mismatch, access denied, etc.)
    Blocked,
    /// Mission not feasible as specified (wrong assumptions in request)
    NotFeasible,
}

impl std::fmt::Display for MissionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Active => write!(f, "active"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Blocked => write!(f, "blocked"),
            Self::NotFeasible => write!(f, "not_feasible"),
            Self::Interrupted => write!(f, "interrupted"),
        }
    }
}

// Mission and MissionHistoryEntry are now defined in mission_store module

/// Metadata for a desktop session started during a mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopSessionInfo {
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshots_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The mission that owns this desktop session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<uuid::Uuid>,
    /// Timestamp until which the session should be kept alive (ISO 8601).
    /// User can extend this to prevent auto-close.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_until: Option<String>,
}

/// Request to set mission status.
#[derive(Debug, Clone, Deserialize)]
pub struct SetMissionStatusRequest {
    pub status: MissionStatus,
}

/// Request to rename a mission.
#[derive(Debug, Clone, Deserialize)]
pub struct SetMissionTitleRequest {
    pub title: String,
}

// MissionStore trait and implementations are in mission_store module

/// Shared tool hub used to await frontend tool results.
///
/// Supports both orderings:
/// - register-then-resolve (normal flow)
/// - resolve-then-register (frontend submits answer before backend registers)
#[derive(Debug)]
pub struct FrontendToolHub {
    pending: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    early_results: Mutex<HashMap<String, serde_json::Value>>,
}

impl FrontendToolHub {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            early_results: Mutex::new(HashMap::new()),
        }
    }

    /// Register a tool call that expects a frontend-provided result.
    /// If the result was already submitted (resolve-before-register), it is
    /// delivered immediately.
    pub async fn register(&self, tool_call_id: String) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();

        {
            let mut early = self.early_results.lock().await;
            if let Some(result) = early.remove(&tool_call_id) {
                let _ = tx.send(result);
                return rx;
            }
        }

        let mut pending = self.pending.lock().await;
        pending.insert(tool_call_id, tx);
        rx
    }

    /// Resolve a pending tool call by id.
    /// If no one has registered yet, the result is cached for later pickup.
    pub async fn resolve(&self, tool_call_id: &str, result: serde_json::Value) -> Result<(), ()> {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(tool_call_id) {
            let _ = tx.send(result);
            return Ok(());
        }
        drop(pending);

        let mut early = self.early_results.lock().await;
        const MAX_EARLY_RESULTS: usize = 256;
        if early.len() >= MAX_EARLY_RESULTS {
            tracing::warn!(
                "FrontendToolHub: early_results cache full ({} entries), dropping an entry",
                early.len()
            );
            if let Some(key) = early.keys().next().cloned() {
                early.remove(&key);
            }
        }
        early.insert(tool_call_id.to_string(), result);
        Ok(())
    }
}

impl Default for FrontendToolHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Control session runtime stored in `AppState`.
#[derive(Clone)]
pub struct ControlState {
    pub cmd_tx: mpsc::Sender<ControlCommand>,
    pub events_tx: broadcast::Sender<AgentEvent>,
    pub tool_hub: Arc<FrontendToolHub>,
    pub status: Arc<RwLock<ControlStatus>>,
    /// Current mission ID (if any) - primary mission in the old sequential model
    pub current_mission: Arc<RwLock<Option<Uuid>>>,
    /// Current agent tree snapshot (for refresh resilience)
    pub current_tree: Arc<RwLock<Option<AgentTreeNode>>>,
    /// Current execution progress (for progress indicator)
    pub progress: Arc<RwLock<ExecutionProgress>>,
    /// Running missions (for parallel execution)
    pub running_missions: Arc<RwLock<Vec<super::mission_runner::RunningMissionInfo>>>,
    /// Max parallel missions allowed
    pub max_parallel: usize,
    /// Mission persistence (SQLite-backed)
    pub mission_store: Arc<dyn MissionStore>,
    /// Cache for semantic mission search results keyed by normalized query hash
    pub mission_search_cache: Arc<RwLock<HashMap<u64, MissionSearchCacheEntry>>>,
}

/// Control session manager for per-user sessions.
#[derive(Clone)]
pub struct ControlHub {
    sessions: Arc<RwLock<HashMap<String, ControlState>>>,
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    secrets: Option<Arc<SecretsStore>>,
    telegram_bridge: Option<super::telegram::SharedTelegramBridge>,
}

impl ControlHub {
    pub fn new(
        config: Config,
        root_agent: AgentRef,
        mcp: Arc<McpRegistry>,
        workspaces: workspace::SharedWorkspaceStore,
        library: SharedLibrary,
        secrets: Option<Arc<SecretsStore>>,
    ) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            config,
            root_agent,
            mcp,
            workspaces,
            library,
            secrets,
            telegram_bridge: None,
        }
    }

    /// Set the Telegram bridge reference (called after AppState is created).
    pub fn set_telegram_bridge(&mut self, bridge: super::telegram::SharedTelegramBridge) {
        self.telegram_bridge = Some(bridge);
    }

    /// Get the events broadcast sender from any active session.
    /// Used by the FIDO signing hub to broadcast signing requests to all
    /// connected SSE clients regardless of which user session they belong to.
    pub fn get_any_session_events_tx(&self) -> Option<broadcast::Sender<AgentEvent>> {
        // Try to read without blocking — best-effort for the FIDO relay.
        let sessions = self.sessions.try_read().ok()?;
        sessions.values().next().map(|s| s.events_tx.clone())
    }

    pub async fn get_or_spawn(&self, user: &AuthUser) -> ControlState {
        if let Some(existing) = self.sessions.read().await.get(&user.id).cloned() {
            return existing;
        }
        let mut sessions = self.sessions.write().await;
        if let Some(existing) = sessions.get(&user.id).cloned() {
            return existing;
        }

        // Get mission store type from environment (default: SQLite)
        let store_type = std::env::var("MISSION_STORE_TYPE")
            .map(|s| MissionStoreType::from_str(&s))
            .unwrap_or(MissionStoreType::Sqlite);

        let base_dir = self
            .config
            .working_dir
            .join(".sandboxed-sh")
            .join("missions");
        let mission_store: Arc<dyn MissionStore> =
            match create_mission_store(store_type, base_dir, &user.id).await {
                Ok(store) => Arc::from(store),
                Err(err) => {
                    tracing::warn!(
                        "Failed to initialize {:?} mission store, falling back to memory: {}",
                        store_type,
                        err
                    );
                    Arc::new(mission_store::InMemoryMissionStore::new())
                }
            };

        let state = spawn_control_session(
            self.config.clone(),
            Arc::clone(&self.root_agent),
            Arc::clone(&self.mcp),
            Arc::clone(&self.workspaces),
            Arc::clone(&self.library),
            mission_store,
            self.secrets.clone(),
            self.telegram_bridge.clone(),
        );
        sessions.insert(user.id.clone(), state.clone());

        // Drop the write lock before performing async I/O so concurrent
        // callers of get_or_spawn / all_sessions are not blocked.
        drop(sessions);

        // Boot Telegram channels for this user's missions — ensures channels
        // are registered before we start serving requests.
        if let Some(ref bridge) = self.telegram_bridge {
            let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
                .unwrap_or_else(|_| format!("http://{}:{}", self.config.host, self.config.port));
            bridge
                .boot_from_store(
                    &state.mission_store,
                    state.cmd_tx.clone(),
                    state.events_tx.clone(),
                    &public_url,
                )
                .await;
        }

        state
    }

    pub async fn all_sessions(&self) -> Vec<ControlState> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// Get a mission store for desktop management.
    /// Uses the default user's store if available, or creates a temporary one.
    pub async fn get_mission_store(&self) -> Arc<dyn MissionStore> {
        // Try to get from the first existing session
        if let Some(session) = self.sessions.read().await.values().next() {
            return Arc::clone(&session.mission_store);
        }

        // No existing sessions, create a temporary store
        let store_type = std::env::var("MISSION_STORE_TYPE")
            .map(|s| MissionStoreType::from_str(&s))
            .unwrap_or(MissionStoreType::Sqlite);

        let base_dir = self
            .config
            .working_dir
            .join(".sandboxed-sh")
            .join("missions");

        let user = crate::api::auth::implicit_single_tenant_user(&self.config);
        match create_mission_store(store_type, base_dir, &user.id).await {
            Ok(store) => Arc::from(store),
            Err(err) => {
                tracing::warn!(
                    "Failed to create mission store for desktop management: {}",
                    err
                );
                Arc::new(mission_store::InMemoryMissionStore::new())
            }
        }
    }
}

/// Execution progress for showing overall mission progress
#[derive(Debug, Clone, Serialize, Default)]
pub struct ExecutionProgress {
    /// Total number of subtasks
    pub total_subtasks: usize,
    /// Number of completed subtasks
    pub completed_subtasks: usize,
    /// Currently executing subtask description (if any)
    pub current_subtask: Option<String>,
    /// Current depth level (0=root, 1=subtask, 2=sub-subtask)
    pub current_depth: u8,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ControlStatus {
    pub state: ControlRunState,
    pub queue_len: usize,
    pub mission_id: Option<Uuid>,
}

async fn set_and_emit_status(
    status: &Arc<RwLock<ControlStatus>>,
    events: &broadcast::Sender<AgentEvent>,
    state: ControlRunState,
    queue_len: usize,
    mission_id: Option<Uuid>,
) {
    {
        let mut s = status.write().await;
        s.state = state;
        s.queue_len = queue_len;
        s.mission_id = mission_id;
    }
    let _ = events.send(AgentEvent::Status {
        state,
        queue_len,
        mission_id,
    });
}

async fn control_for_user(state: &Arc<AppState>, user: &AuthUser) -> ControlState {
    state.control.get_or_spawn(user).await
}

/// Enqueue a user message for the global control session.
/// If mission_id is provided and differs from the currently running mission,
/// the backend will automatically start it in parallel (if capacity allows).
pub async fn post_message(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<ControlMessageRequest>,
) -> Result<Json<ControlMessageResponse>, (StatusCode, String)> {
    let content = req.content.trim().to_string();
    if content.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "content is required".to_string()));
    }

    let id = Uuid::new_v4();
    let agent = req.agent;
    let target_mission_id = req.mission_id;
    let control = control_for_user(&state, &user).await;
    let (queued_tx, queued_rx) = oneshot::channel();
    tracing::info!(
        user_id = %user.id,
        username = %user.username,
        message_id = %id,
        content_len = content.len(),
        agent = ?agent,
        target_mission_id = ?target_mission_id,
        "Received control message"
    );
    control
        .cmd_tx
        .send(ControlCommand::UserMessage {
            id,
            content,
            agent,
            target_mission_id,
            respond: queued_tx,
        })
        .await
        .map_err(session_unavailable)?;
    let queued = match queued_rx.await {
        Ok(value) => value,
        Err(_) => {
            let status = control.status.read().await;
            status.state != ControlRunState::Idle
        }
    };
    Ok(Json(ControlMessageResponse { id, queued }))
}

/// Submit a frontend tool result to resume the running agent.
pub async fn post_tool_result(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<ControlToolResultRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if req.tool_call_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "tool_call_id is required".to_string(),
        ));
    }
    if req.name.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name is required".to_string()));
    }

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::ToolResult {
            tool_call_id: req.tool_call_id,
            name: req.name,
            result: req.result,
        })
        .await
        .map_err(session_unavailable)?;

    Ok(ok_json())
}

/// Cancel the currently running control session task.
pub async fn post_cancel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::Cancel)
        .await
        .map_err(session_unavailable)?;
    Ok(ok_json())
}

// ==================== Queue Management Endpoints ====================

/// Get the current message queue.
pub async fn get_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<QueuedMessage>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::GetQueue { respond: tx })
        .await
        .map_err(session_unavailable)?;
    let queue = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to get queue".to_string(),
        )
    })?;
    Ok(Json(queue))
}

/// Remove a message from the queue.
pub async fn remove_from_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(message_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::RemoveFromQueue {
            message_id,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;
    let removed = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to remove from queue".to_string(),
        )
    })?;
    if removed {
        Ok(ok_json())
    } else {
        Err((StatusCode::NOT_FOUND, "message not in queue".to_string()))
    }
}

/// Clear all messages from the queue.
pub async fn clear_queue(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let (tx, rx) = oneshot::channel();
    control
        .cmd_tx
        .send(ControlCommand::ClearQueue { respond: tx })
        .await
        .map_err(session_unavailable)?;
    let cleared = rx.await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to clear queue".to_string(),
        )
    })?;
    Ok(Json(serde_json::json!({ "ok": true, "cleared": cleared })))
}

// ==================== Mission Endpoints ====================

/// List all missions.
pub async fn list_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<Mission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let mut missions = control
        .mission_store
        .list_missions(50, 0)
        .await
        .map_err(internal_error)?;
    populate_workspace_names(&state, &mut missions).await;
    Ok(Json(missions))
}

async fn populate_workspace_names(state: &Arc<AppState>, missions: &mut [Mission]) {
    for mission in missions {
        if let Some(workspace) = state.workspaces.get(mission.workspace_id).await {
            mission.workspace_name = Some(workspace.name);
        }
    }
}

#[derive(Debug, Clone)]
struct MissionSearchCandidate {
    mission: Mission,
    relevance_score: f64,
}

async fn list_missions_for_search(
    state: &Arc<AppState>,
    control: &ControlState,
    query: &str,
    limit: usize,
) -> Result<Vec<MissionSearchCandidate>, (StatusCode, String)> {
    const SEARCH_PAGE_SIZE_MIN: usize = 50;
    const SEARCH_PAGE_SIZE_MAX: usize = 200;
    const SEARCH_TARGET_MATCH_MULTIPLIER: usize = 8;
    const SEARCH_MAX_SCAN: usize = 10_000;

    let page_size = (limit.saturating_mul(5)).clamp(SEARCH_PAGE_SIZE_MIN, SEARCH_PAGE_SIZE_MAX);
    let target_matches = limit
        .saturating_mul(SEARCH_TARGET_MATCH_MULTIPLIER)
        .max(limit);

    let mut all = Vec::new();
    let mut offset = 0;
    let mut matched_total = 0usize;

    while offset < SEARCH_MAX_SCAN {
        let mut page = control
            .mission_store
            .list_missions(page_size, offset)
            .await
            .map_err(internal_error)?;
        if page.is_empty() {
            break;
        }
        populate_workspace_names(state, &mut page).await;
        let page_len = page.len();
        let mut matched_in_page = 0usize;
        for mission in page {
            let relevance_score =
                mission_search_relevance_score(&mission, query, mission.workspace_name.as_deref());
            if relevance_score > 0.0 {
                matched_in_page += 1;
            }
            all.push(MissionSearchCandidate {
                mission,
                relevance_score,
            });
        }
        matched_total += matched_in_page;
        if page_len < page_size {
            break;
        }
        offset += page_size;
        if matched_total >= target_matches {
            break;
        }
    }

    if offset >= SEARCH_MAX_SCAN {
        tracing::warn!(
            "Mission search scan hit cap ({} missions); some older missions may not be searched",
            SEARCH_MAX_SCAN
        );
    }

    Ok(all)
}

#[derive(Debug, Deserialize)]
pub struct SearchMissionsQuery {
    pub q: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SearchMissionMomentsQuery {
    pub q: String,
    pub mission_id: Option<Uuid>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissionSearchResult {
    pub mission: Mission,
    pub relevance_score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissionMomentSearchResult {
    pub mission: Mission,
    pub entry_index: usize,
    pub role: String,
    pub snippet: String,
    pub rationale: String,
    pub relevance_score: f64,
}

#[derive(Debug, Clone)]
pub struct MissionSearchCacheEntry {
    pub cached_at: std::time::Instant,
    pub freshness_key: u64,
    pub recency_fingerprint: u64,
    pub results: Vec<MissionSearchResult>,
}

const MISSION_SEARCH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

fn mission_search_cache_hit_by_ttl(
    entry: &MissionSearchCacheEntry,
    recency_fingerprint: u64,
) -> bool {
    entry.cached_at.elapsed() <= MISSION_SEARCH_CACHE_TTL
        && entry.recency_fingerprint == recency_fingerprint
}

fn mission_search_cache_hit_by_freshness(
    entry: &MissionSearchCacheEntry,
    recency_fingerprint: u64,
    freshness_key: u64,
) -> bool {
    entry.recency_fingerprint == recency_fingerprint && entry.freshness_key == freshness_key
}

fn mission_search_query_hash(query: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalize_metadata_text(query).hash(&mut hasher);
    hasher.finish()
}

fn mission_search_freshness_key(missions: &[MissionSearchCandidate], page_size: usize) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    missions.len().hash(&mut hasher);
    page_size.hash(&mut hasher);
    for candidate in missions {
        let mission = &candidate.mission;
        mission.id.hash(&mut hasher);
        mission.updated_at.hash(&mut hasher);
        mission.metadata_updated_at.hash(&mut hasher);
        mission.workspace_name.hash(&mut hasher);
    }
    hasher.finish()
}

async fn mission_search_recency_fingerprint(
    mission_store: &Arc<dyn MissionStore>,
) -> Result<u64, String> {
    const MISSION_SEARCH_RECENCY_FINGERPRINT_LIMIT: usize = 64;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let recent = mission_store
        .list_missions(MISSION_SEARCH_RECENCY_FINGERPRINT_LIMIT, 0)
        .await?;
    recent.len().hash(&mut hasher);
    for mission in recent {
        mission.id.hash(&mut hasher);
        mission.updated_at.hash(&mut hasher);
        mission.metadata_updated_at.hash(&mut hasher);
        mission.title.hash(&mut hasher);
        mission.short_description.hash(&mut hasher);
        mission.workspace_name.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Search missions with semantic-aware ranking.
pub async fn search_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<SearchMissionsQuery>,
) -> Result<Json<Vec<MissionSearchResult>>, (StatusCode, String)> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let query_hash = mission_search_query_hash(query);
    let control = control_for_user(&state, &user).await;
    let recency_fingerprint = mission_search_recency_fingerprint(&control.mission_store)
        .await
        .map_err(internal_error)?;
    if let Some(cached_results) = {
        let cache = control.mission_search_cache.read().await;
        cache.get(&query_hash).and_then(|entry| {
            if mission_search_cache_hit_by_ttl(entry, recency_fingerprint) {
                Some(entry.results.clone())
            } else {
                None
            }
        })
    } {
        let mut results = cached_results;
        results.truncate(limit);
        return Ok(Json(results));
    }

    let page_size = (limit.saturating_mul(5)).clamp(50, 200);
    let mission_candidates = list_missions_for_search(&state, &control, query, limit).await?;
    let freshness_key = mission_search_freshness_key(&mission_candidates, page_size);

    if let Some(cached_results) = {
        let cache = control.mission_search_cache.read().await;
        cache.get(&query_hash).and_then(|entry| {
            if mission_search_cache_hit_by_freshness(entry, recency_fingerprint, freshness_key) {
                Some(entry.results.clone())
            } else {
                None
            }
        })
    } {
        let mut results = cached_results;
        results.truncate(limit);
        return Ok(Json(results));
    }

    let mut scored: Vec<MissionSearchResult> = mission_candidates
        .into_iter()
        .filter_map(|candidate| {
            if candidate.relevance_score > 0.0 {
                Some(MissionSearchResult {
                    mission: candidate.mission,
                    relevance_score: candidate.relevance_score,
                })
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.relevance_score
            .total_cmp(&a.relevance_score)
            .then_with(|| b.mission.updated_at.cmp(&a.mission.updated_at))
    });
    {
        const MISSION_SEARCH_CACHE_MAX_ENTRIES: usize = 128;
        let mut cache = control.mission_search_cache.write().await;
        if cache.len() >= MISSION_SEARCH_CACHE_MAX_ENTRIES {
            if let Some(key_to_drop) = cache.keys().next().copied() {
                cache.remove(&key_to_drop);
            }
        }
        cache.insert(
            query_hash,
            MissionSearchCacheEntry {
                cached_at: std::time::Instant::now(),
                freshness_key,
                recency_fingerprint,
                results: scored.clone(),
            },
        );
    }

    scored.truncate(limit);
    Ok(Json(scored))
}

/// Search mission history and return the best matching moment per mission.
pub async fn search_mission_moments(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<SearchMissionMomentsQuery>,
) -> Result<Json<Vec<MissionMomentSearchResult>>, (StatusCode, String)> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let limit = params.limit.unwrap_or(10).clamp(1, 50);
    let control = control_for_user(&state, &user).await;

    let mut missions: Vec<Mission> = if let Some(mission_id) = params.mission_id {
        let Some(mission) = control
            .mission_store
            .get_mission(mission_id)
            .await
            .map_err(internal_error)?
        else {
            return Ok(Json(Vec::new()));
        };
        vec![mission]
    } else {
        const SEARCH_PAGE_SIZE: usize = 100;
        const SEARCH_MAX_SCAN: usize = 2_000;
        let mut all = Vec::new();
        let mut offset = 0usize;
        while offset < SEARCH_MAX_SCAN {
            let page = control
                .mission_store
                .list_missions(SEARCH_PAGE_SIZE, offset)
                .await
                .map_err(internal_error)?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            all.extend(page);
            if page_len < SEARCH_PAGE_SIZE {
                break;
            }
            offset += SEARCH_PAGE_SIZE;
        }
        if offset >= SEARCH_MAX_SCAN {
            tracing::warn!(
                "Mission moment search scan hit cap ({} missions); some older missions may not be searched",
                SEARCH_MAX_SCAN
            );
        }
        all
    };

    populate_workspace_names(&state, &mut missions).await;

    let mut results: Vec<MissionMomentSearchResult> = missions
        .into_iter()
        .filter_map(|mission| {
            let best = best_mission_moment(&mission, query)?;
            Some(MissionMomentSearchResult {
                mission,
                entry_index: best.entry_index,
                role: best.role,
                snippet: best.snippet,
                rationale: best.rationale,
                relevance_score: best.relevance_score,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.relevance_score
            .total_cmp(&a.relevance_score)
            .then_with(|| b.mission.updated_at.cmp(&a.mission.updated_at))
            .then_with(|| a.entry_index.cmp(&b.entry_index))
    });
    results.truncate(limit);
    Ok(Json(results))
}

/// Get a specific mission.
pub async fn get_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    match control
        .mission_store
        .get_mission(id)
        .await
        .map_err(internal_error)?
    {
        Some(mut mission) => {
            // Populate workspace_name
            if let Some(workspace) = state.workspaces.get(mission.workspace_id).await {
                mission.workspace_name = Some(workspace.name);
            }
            Ok(Json(mission))
        }
        None => Err((StatusCode::NOT_FOUND, format!("Mission {} not found", id))),
    }
}

/// Create a new mission and switch to it.
/// Request body for creating a mission
#[derive(Debug, Deserialize)]
pub struct CreateMissionRequest {
    pub title: Option<String>,
    /// Workspace ID to run the mission in (defaults to host workspace)
    pub workspace_id: Option<Uuid>,
    /// Agent name from library (e.g., "code-reviewer")
    pub agent: Option<String>,
    /// Optional model override (provider/model) - deprecated, use config_profile instead
    pub model_override: Option<String>,
    /// Optional model effort override (supports: low, medium, high, xhigh, max)
    pub model_effort: Option<String>,
    /// Config profile to use for this mission (overrides workspace's default profile)
    pub config_profile: Option<String>,
    /// Backend to use for this mission ("opencode" or "claudecode")
    pub backend: Option<String>,
    /// Parent mission ID (for orchestrated worker missions)
    pub parent_mission_id: Option<Uuid>,
    /// Working directory override (for git worktrees etc.)
    pub working_directory: Option<String>,
}

fn deserialize_string_patch<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct UpdateMissionSettingsRequest {
    /// Backend to use on the next turn ("opencode", "claudecode", "codex", etc.).
    pub backend: Option<String>,
    /// Agent name. Omit to leave unchanged, null/empty string to clear.
    #[serde(default, deserialize_with = "deserialize_string_patch")]
    pub agent: Option<Option<String>>,
    /// Model override. Omit to leave unchanged, null/empty string to clear.
    #[serde(default, deserialize_with = "deserialize_string_patch")]
    pub model_override: Option<Option<String>>,
    /// Model effort. Omit to leave unchanged, null/empty string to clear.
    #[serde(default, deserialize_with = "deserialize_string_patch")]
    pub model_effort: Option<Option<String>>,
    /// Config profile. Omit to leave unchanged, null/empty string to clear.
    #[serde(default, deserialize_with = "deserialize_string_patch")]
    pub config_profile: Option<Option<String>>,
}

fn normalize_model_effort(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "low" => Some("low".to_string()),
        "medium" => Some("medium".to_string()),
        "high" => Some("high".to_string()),
        "xhigh" => Some("xhigh".to_string()),
        "max" => Some("max".to_string()),
        _ => None,
    }
}

fn normalize_model_effort_for_backend(backend: Option<&str>, raw: &str) -> Option<String> {
    let normalized = normalize_model_effort(raw)?;
    match (backend, normalized.as_str()) {
        (Some("claudecode"), "low" | "medium" | "high" | "xhigh" | "max") => Some(normalized),
        (Some("codex"), "low" | "medium" | "high") => Some(normalized),
        _ => None,
    }
}

fn supported_model_efforts_for_backend(backend: Option<&str>) -> &'static str {
    match backend {
        Some("claudecode") => "low, medium, high, xhigh, max",
        Some("codex") => "low, medium, high",
        _ => "none",
    }
}

fn normalize_string_patch(value: Option<Option<String>>) -> Option<Option<String>> {
    value.map(|inner| {
        inner.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    })
}

fn normalize_model_override_for_backend(backend: Option<&str>, raw_model: &str) -> Option<String> {
    let trimmed = raw_model.trim();
    if trimmed.is_empty() {
        return None;
    }
    if backend != Some("opencode") {
        if let Some((_, model_id)) = trimmed.split_once('/') {
            return Some(model_id.to_string());
        }
    }
    Some(trimmed.to_string())
}

pub async fn create_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    body: Option<Json<CreateMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let req = body.map(|b| b.0).unwrap_or(CreateMissionRequest {
        title: None,
        workspace_id: None,
        agent: None,
        model_override: None,
        model_effort: None,
        config_profile: None,
        backend: None,
        parent_mission_id: None,
        working_directory: None,
    });

    let title = req.title.clone();
    let workspace_id = req.workspace_id;
    let agent = req.agent.clone();
    let config_profile = req.config_profile.clone();
    let mut backend = req.backend.clone();
    let mut model_override = req.model_override.clone();
    let mut model_effort = req.model_effort.clone();
    if let Some(value) = backend.as_ref() {
        if value.trim().is_empty() {
            backend = None;
        }
    }
    if let Some(value) = model_override.as_ref() {
        if value.trim().is_empty() {
            model_override = None;
        }
    }
    if let Some(value) = model_effort.as_ref() {
        if value.trim().is_empty() {
            model_effort = None;
        }
    }

    // If no backend specified, use the default from registry
    // This needs to happen BEFORE agent validation so we validate against the correct backend
    if backend.is_none() {
        let registry = state.backend_registry.read().await;
        backend = Some(registry.default_id().to_string());
    }

    // Model effort is supported for Codex and Claude Code missions.
    if !matches!(backend.as_deref(), Some("codex") | Some("claudecode")) {
        model_effort = None;
    } else if let Some(value) = model_effort.as_ref() {
        model_effort = normalize_model_effort_for_backend(backend.as_deref(), value);
        if model_effort.is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid model_effort for backend '{}'. Supported values: {}",
                    backend.as_deref().unwrap_or("unknown"),
                    supported_model_efforts_for_backend(backend.as_deref())
                ),
            ));
        }
    }

    // Normalize model override based on backend expectations.
    // OpenCode expects provider/model; Claude Code and Codex expect raw model IDs.
    if let Some(ref raw_model) = model_override {
        model_override = normalize_model_override_for_backend(backend.as_deref(), raw_model);
    }

    // Resolve the effective config profile:
    // 1. Use explicit config_profile from request if provided
    // 2. Otherwise use workspace's config_profile
    // 3. Fall back to "default"
    let effective_config_profile = if let Some(ref profile) = config_profile {
        Some(profile.clone())
    } else if let Some(ws_id) = workspace_id {
        state
            .workspaces
            .get(ws_id)
            .await
            .and_then(|ws| ws.config_profile)
    } else {
        None
    };

    // Validate agent exists before creating mission (fail fast with clear error)
    // Skip validation for Claude Code, Amp, Codex, and Gemini - they have their own built-in agents
    if let Some(ref agent_name) = agent {
        let backend_id = backend.as_deref();
        let skip_validation = matches!(backend_id, Some("claudecode" | "amp" | "codex" | "gemini"));
        if !skip_validation {
            super::library::validate_agent_exists(
                &state,
                agent_name,
                effective_config_profile.as_deref(),
            )
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        }
    }

    // Validate backend exists
    if let Some(ref backend_id) = backend {
        let registry = state.backend_registry.read().await;
        if registry.get(backend_id).is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Unknown backend: {}", backend_id),
            ));
        }
    }

    // Validate model override if provided
    if let Some(ref model) = model_override {
        let backend_id = backend.as_deref().unwrap_or("claudecode");
        if let Err(e) = super::providers::validate_model_override(&state, backend_id, model).await {
            return Err((StatusCode::BAD_REQUEST, e));
        }
    }

    // If no model_override specified, resolve from config profile for Claude Code
    if backend.as_deref() == Some("claudecode") && model_override.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&state.library, effective_config_profile.as_deref())
                .await
        {
            model_override = Some(default_model);
        }
    }

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::CreateMission {
            title,
            workspace_id,
            agent,
            model_override,
            model_effort,
            backend,
            config_profile: effective_config_profile,
            parent_mission_id: req.parent_mission_id,
            working_directory: req.working_directory,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(Json)
        .map_err(internal_error)
}

/// Update mission run settings for future turns.
pub async fn update_mission_settings(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMissionSettingsRequest>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let current = control
        .mission_store
        .get_mission(id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Mission {} not found", id)))?;

    let backend = req.backend.as_ref().and_then(|backend| {
        let trimmed = backend.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let backend_changed = backend
        .as_deref()
        .is_some_and(|backend| backend != current.backend);
    let effective_backend = backend.clone().unwrap_or_else(|| current.backend.clone());

    {
        let registry = state.backend_registry.read().await;
        if registry.get(&effective_backend).is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Unknown backend: {}", effective_backend),
            ));
        }
    }

    let agent = normalize_string_patch(req.agent);
    let mut model_override = normalize_string_patch(req.model_override);
    let mut model_effort = normalize_string_patch(req.model_effort);
    let config_profile = normalize_string_patch(req.config_profile);

    if backend_changed && model_override.is_none() {
        model_override = Some(None);
    }
    if backend_changed && model_effort.is_none() {
        model_effort = Some(None);
    }
    if effective_backend == "amp" {
        model_override = Some(None);
    }
    if !matches!(effective_backend.as_str(), "codex" | "claudecode") {
        model_effort = Some(None);
    }

    let workspace_config_profile = state
        .workspaces
        .get(current.workspace_id)
        .await
        .and_then(|ws| ws.config_profile);
    let effective_config_profile = match &config_profile {
        Some(Some(profile)) => Some(profile.clone()),
        Some(None) => workspace_config_profile.clone(),
        None => current
            .config_profile
            .clone()
            .or_else(|| workspace_config_profile.clone()),
    };

    let effective_agent = match &agent {
        Some(Some(agent)) => Some(agent.clone()),
        Some(None) => None,
        None => current.agent.clone(),
    };
    if let Some(ref agent_name) = effective_agent {
        let skip_validation = matches!(
            effective_backend.as_str(),
            "claudecode" | "amp" | "codex" | "gemini"
        );
        if !skip_validation {
            super::library::validate_agent_exists(
                &state,
                agent_name,
                effective_config_profile.as_deref(),
            )
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        }
    }

    if let Some(Some(raw_effort)) = model_effort.as_ref() {
        let normalized = normalize_model_effort_for_backend(Some(&effective_backend), raw_effort)
            .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!(
                    "Invalid model_effort for backend '{}'. Supported values: {}",
                    effective_backend,
                    supported_model_efforts_for_backend(Some(&effective_backend))
                ),
            )
        })?;
        model_effort = Some(Some(normalized));
    }

    let mut effective_model = match &model_override {
        Some(Some(model)) => normalize_model_override_for_backend(Some(&effective_backend), model),
        Some(None) => None,
        None => current.model_override.as_deref().and_then(|model| {
            normalize_model_override_for_backend(Some(&effective_backend), model)
        }),
    };
    if let Some(ref model) = effective_model {
        if model_override.as_ref().and_then(|value| value.as_ref()) != Some(model) {
            model_override = Some(Some(model.clone()));
        }
    }

    if effective_backend == "claudecode" && effective_model.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&state.library, effective_config_profile.as_deref())
                .await
        {
            effective_model = Some(default_model.clone());
            model_override = Some(Some(default_model));
        }
    }

    if let Some(ref model) = effective_model {
        if let Err(e) =
            super::providers::validate_model_override(&state, &effective_backend, model).await
        {
            return Err((StatusCode::BAD_REQUEST, e));
        }
    }

    let (tx, rx) = oneshot::channel();
    let session_id = Uuid::new_v4().to_string();
    control
        .cmd_tx
        .send(ControlCommand::UpdateMissionSettings {
            id,
            backend,
            agent,
            model_override,
            model_effort,
            config_profile,
            session_id,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await.map_err(recv_failed)?.map(Json).map_err(|e| {
        if e.contains("not found") {
            (StatusCode::NOT_FOUND, e)
        } else if e.contains("running") {
            (StatusCode::CONFLICT, e)
        } else {
            internal_error(e)
        }
    })
}

/// Load/switch to a mission.
pub async fn load_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::LoadMission { id, respond: tx })
        .await
        .map_err(session_unavailable)?;

    rx.await.map_err(recv_failed)?.map(Json).map_err(|e| {
        // Return 404 if mission was not found
        if e.contains("not found") {
            (StatusCode::NOT_FOUND, e)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

/// Set mission status (completed/failed).
pub async fn set_mission_status(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMissionStatusRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::SetMissionStatus {
            id,
            status: req.status,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(|_| ok_json())
        .map_err(internal_error)
}

/// Set mission title (rename mission).
pub async fn set_mission_title(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMissionTitleRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::SetMissionTitle {
            id,
            title: req.title,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(|_| ok_json())
        .map_err(internal_error)
}

/// Get the current mission (if any).
pub async fn get_current_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Option<Mission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let current_id = *control.current_mission.read().await;

    match current_id {
        Some(id) => {
            let mission = control
                .mission_store
                .get_mission(id)
                .await
                .map_err(internal_error)?;
            Ok(Json(mission))
        }
        None => Ok(Json(None)),
    }
}

/// Get current agent tree snapshot (for refresh resilience).
/// Returns the last emitted tree state, or null if no tree is active.
pub async fn get_tree(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<Option<AgentTreeNode>> {
    let control = control_for_user(&state, &user).await;
    let tree = control.current_tree.read().await.clone();
    Json(tree)
}

/// Get tree for a specific mission.
/// For currently running mission, returns the live tree from memory.
/// For completed missions, returns the saved final_tree from the database.
pub async fn get_mission_tree(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Option<AgentTreeNode>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    // Check if this is the current active mission
    let current_id = *control.current_mission.read().await;
    if current_id == Some(mission_id) {
        // Return live tree from memory
        let tree = control.current_tree.read().await.clone();
        return Ok(Json(tree));
    }
    let tree = control
        .mission_store
        .get_mission_tree(mission_id)
        .await
        .map_err(internal_error)?;
    if tree.is_some() {
        return Ok(Json(tree));
    }

    let mission_exists = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal_error)?;
    if mission_exists.is_some() {
        Ok(Json(None))
    } else {
        Err((StatusCode::NOT_FOUND, "Mission not found".to_string()))
    }
}

/// Get current execution progress (for progress indicator).
pub async fn get_progress(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Json<ExecutionProgress> {
    let control = control_for_user(&state, &user).await;
    let progress = control.progress.read().await.clone();
    Json(progress)
}

/// Query params for events endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct GetEventsQuery {
    /// Comma-separated event types to filter (e.g., "tool_call,tool_result")
    #[serde(default)]
    pub types: Option<String>,
    /// Maximum number of events to return
    #[serde(default)]
    pub limit: Option<usize>,
    /// Offset for pagination
    #[serde(default)]
    pub offset: Option<usize>,
    /// When true, return the latest N events (computes offset from total count)
    #[serde(default)]
    pub latest: Option<bool>,
    /// If set, return only events with `sequence > since_seq`, ordered
    /// by sequence ASC. Used by the client for delta reconnect to
    /// avoid redownloading the full event tail on every focus/reopen.
    /// Takes precedence over `offset`/`latest` when provided.
    #[serde(default)]
    pub since_seq: Option<i64>,
    /// If set, return only events with `sequence < before_seq`, ordered
    /// by sequence ASC (oldest first). Used by the client for backwards
    /// pagination — pass the lowest sequence already seen to fetch the
    /// next page of older events. Takes precedence over `since_seq` /
    /// `offset` / `latest` when provided.
    #[serde(default)]
    pub before_seq: Option<i64>,
}

/// Get events for a mission (for debugging/replay).
///
/// Response includes `X-Total-Events` (total count matching the type
/// filter) and `X-Max-Sequence` (highest sequence stored for this
/// mission) headers so the client can decide whether it's caught up
/// without issuing a second request.
pub async fn get_mission_events(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<GetEventsQuery>,
) -> Result<Response, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    // Check mission exists
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal_error)?;

    if mission.is_none() {
        return Err((StatusCode::NOT_FOUND, "Mission not found".to_string()));
    }

    // Parse event types filter
    let types: Option<Vec<&str>> = query
        .types
        .as_ref()
        .map(|s| s.split(',').map(|t| t.trim()).collect());

    let events = if let Some(before_seq) = query.before_seq {
        control
            .mission_store
            .get_events_before(mission_id, before_seq, types.as_deref(), query.limit)
            .await
            .map_err(internal_error)?
    } else if let Some(since_seq) = query.since_seq {
        control
            .mission_store
            .get_events_since(mission_id, since_seq, types.as_deref(), query.limit)
            .await
            .map_err(internal_error)?
    } else {
        // When latest=true with a limit, compute offset to return the last N events
        let offset = if query.latest.unwrap_or(false) {
            if let Some(limit) = query.limit {
                let total = control
                    .mission_store
                    .count_events(mission_id, types.as_deref())
                    .await
                    .map_err(internal_error)?;
                Some(total.saturating_sub(limit))
            } else {
                query.offset
            }
        } else {
            query.offset
        };

        control
            .mission_store
            .get_events(mission_id, types.as_deref(), query.limit, offset)
            .await
            .map_err(internal_error)?
    };

    // Metadata headers let the client decide whether it's caught up
    // without a second round-trip. Failures here are non-fatal — we just
    // skip the header rather than breaking the whole response.
    let total = control
        .mission_store
        .count_events(mission_id, types.as_deref())
        .await
        .ok();
    let max_seq = control
        .mission_store
        .max_event_sequence(mission_id)
        .await
        .ok();

    let mut response = Json(events).into_response();
    let headers = response.headers_mut();
    if let Some(total) = total {
        if let Ok(v) = header::HeaderValue::from_str(&total.to_string()) {
            headers.insert("X-Total-Events", v);
        }
    }
    if let Some(max_seq) = max_seq {
        if let Ok(v) = header::HeaderValue::from_str(&max_seq.to_string()) {
            headers.insert("X-Max-Sequence", v);
        }
    }
    // CORS exposure so browsers can read these headers from JS.
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("X-Total-Events, X-Max-Sequence"),
    );

    Ok(response)
}

// ==================== Diagnostic Endpoints ====================

/// Response for OpenCode diagnostic endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct OpenCodeDiagnostics {
    /// OpenCode base URL
    pub base_url: String,
    /// Current session ID (if active)
    pub session_id: Option<String>,
    /// Session status from OpenCode
    pub session_status: Option<crate::opencode::OpenCodeSessionStatus>,
    /// Error message if status check failed
    pub error: Option<String>,
}

/// Get OpenCode diagnostics.
/// Note: With per-mission CLI execution, there's no central server to diagnose.
/// This endpoint now returns information about the execution mode.
pub async fn get_opencode_diagnostics(
    State(_state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<OpenCodeDiagnostics>, (StatusCode, String)> {
    // Per-mission CLI execution doesn't use a central server
    Ok(Json(OpenCodeDiagnostics {
        base_url: "per-mission-cli-mode".to_string(),
        session_id: None,
        session_status: None,
        error: Some(
            "Per-mission CLI mode: No central server. Each mission spawns its own CLI process."
                .to_string(),
        ),
    }))
}

// ==================== Parallel Mission Endpoints ====================

/// List currently running missions.
pub async fn list_running_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<super::mission_runner::RunningMissionInfo>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let running = get_running_missions(&control).await?;
    Ok(Json(running))
}

/// Request body for starting a mission in parallel.
#[derive(Debug, Deserialize)]
pub struct StartParallelRequest {
    pub content: String,
}

/// Start a mission in parallel (if capacity allows).
pub async fn start_mission_parallel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<StartParallelRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::StartParallel {
            mission_id,
            content: req.content,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(|_| Json(serde_json::json!({ "ok": true, "mission_id": mission_id })))
        .map_err(|e| (StatusCode::CONFLICT, e))
}

/// Cancel a specific mission.
pub async fn cancel_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::CancelMission {
            mission_id,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(|_| Json(serde_json::json!({ "ok": true, "cancelled": mission_id })))
        .map_err(|e| (StatusCode::NOT_FOUND, e))
}

/// Request body for resuming a mission
#[derive(Debug, Deserialize, Default)]
pub struct ResumeMissionRequest {
    /// If true, clean the mission's work directory before resuming
    #[serde(default)]
    pub clean_workspace: bool,
    /// If true, do not send the automatic resume message.
    /// Useful when the user is about to send their own custom message.
    #[serde(default)]
    pub skip_message: bool,
}

/// Resume an interrupted mission.
/// This reconstructs context from history and work directory, then restarts execution.
pub async fn resume_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    body: Option<Json<ResumeMissionRequest>>,
) -> Result<Json<Mission>, (StatusCode, String)> {
    let (clean_workspace, skip_message) = body
        .map(|b| (b.clean_workspace, b.skip_message))
        .unwrap_or((false, false));
    let (tx, rx) = oneshot::channel();

    let control = control_for_user(&state, &user).await;
    control
        .cmd_tx
        .send(ControlCommand::ResumeMission {
            mission_id,
            clean_workspace,
            skip_message,
            respond: tx,
        })
        .await
        .map_err(session_unavailable)?;

    rx.await
        .map_err(recv_failed)?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Get parallel execution configuration.
pub async fn get_parallel_config(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let running = get_running_missions(&control).await?;
    let max_parallel = crate::settings::max_parallel_missions_cached_or(control.max_parallel);

    Ok(Json(serde_json::json!({
        "max_parallel_missions": max_parallel,
        "running_count": running.len(),
    })))
}

/// Delete a mission by ID.
/// Only allows deleting missions that are not currently running.
pub async fn delete_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let running = get_running_missions(&control).await?;

    if running.iter().any(|m| m.mission_id == mission_id) {
        return Err((
            StatusCode::CONFLICT,
            "Cannot delete a running mission. Cancel it first.".to_string(),
        ));
    }

    let deleted = control
        .mission_store
        .delete_mission(mission_id)
        .await
        .map_err(internal_error)?;

    if deleted {
        clear_mission_metadata_refresh_state(mission_id);
        Ok(Json(serde_json::json!({
            "ok": true,
            "deleted": mission_id
        })))
    } else {
        Err((StatusCode::NOT_FOUND, "Mission not found".to_string()))
    }
}

/// Delete all empty "Untitled" missions.
/// Returns the count of deleted missions.
/// Note: This excludes any currently running missions to prevent data loss.
pub async fn cleanup_empty_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let running = get_running_missions(&control).await?;
    let running_ids: Vec<Uuid> = running.iter().map(|m| m.mission_id).collect();

    let count = control
        .mission_store
        .delete_empty_untitled_missions_excluding(&running_ids)
        .await
        .map_err(internal_error)?;

    if count > 0 {
        clear_stale_mission_metadata_refresh_state(&control.mission_store).await;
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "deleted_count": count
    })))
}

/// Stream control session events via SSE.
pub async fn stream(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let mut rx = control.events_tx.subscribe();
    let stream_id = Uuid::new_v4();
    tracing::info!(
        stream_id = %stream_id,
        user_id = %user.id,
        username = %user.username,
        "Control SSE stream opened"
    );

    // Emit an initial status snapshot immediately.
    let initial = control.status.read().await.clone();

    struct StreamDropGuard {
        stream_id: Uuid,
        user_id: String,
        username: String,
    }

    impl Drop for StreamDropGuard {
        fn drop(&mut self) {
            tracing::info!(
                stream_id = %self.stream_id,
                user_id = %self.user_id,
                username = %self.username,
                "Control SSE stream closed"
            );
        }
    }

    let drop_guard = StreamDropGuard {
        stream_id,
        user_id: user.id.clone(),
        username: user.username.clone(),
    };

    let stream = async_stream::stream! {
        let _guard = drop_guard;
        match Event::default().event("status").json_data(AgentEvent::Status {
            state: initial.state,
            queue_len: initial.queue_len,
            mission_id: initial.mission_id,
        }) {
            Ok(init_ev) => yield Ok(init_ev),
            Err(e) => {
                tracing::error!("Failed to serialize initial SSE status event: {e}");
            }
        }

        // Keepalive interval to prevent connection timeouts during long LLM calls
        let mut keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            let mission_id = ev.mission_id();
                            match &ev {
                                AgentEvent::Thinking { .. } => {
                                    tracing::trace!(
                                        stream_id = %stream_id,
                                        event = %ev.event_name(),
                                        mission_id = ?mission_id,
                                        "Control SSE event"
                                    );
                                }
                                _ => {
                                    tracing::debug!(
                                        stream_id = %stream_id,
                                        event = %ev.event_name(),
                                        mission_id = ?mission_id,
                                        "Control SSE event"
                                    );
                                }
                            }
                            match Event::default().event(ev.event_name()).json_data(&ev) {
                                Ok(sse) => yield Ok(sse),
                                Err(e) => {
                                    tracing::error!(
                                        stream_id = %stream_id,
                                        event = %ev.event_name(),
                                        error = %e,
                                        "Failed to serialize SSE event; dropping"
                                    );
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            tracing::warn!(
                                stream_id = %stream_id,
                                "Control SSE stream lagged; events dropped"
                            );
                            match Event::default()
                                .event("error")
                                .json_data(AgentEvent::Error {
                                    message:
                                        "event stream lagged; some events were dropped"
                                            .to_string(),
                                    mission_id: None,
                                    resumable: false,
                                }) {
                                Ok(sse) => yield Ok(sse),
                                Err(e) => {
                                    tracing::error!(
                                        stream_id = %stream_id,
                                        error = %e,
                                        "Failed to serialize SSE lag error event"
                                    );
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = keepalive_interval.tick() => {
                    // Send SSE comment as keepalive (: comment\n\n)
                    let sse = Event::default().comment("keepalive");
                    yield Ok(sse);
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    ))
}

/// Spawn the global control session actor.
#[allow(clippy::too_many_arguments)]
fn spawn_control_session(
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    mission_store: Arc<dyn MissionStore>,
    secrets: Option<Arc<SecretsStore>>,
    telegram_bridge: Option<super::telegram::SharedTelegramBridge>,
) -> ControlState {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ControlCommand>(256);
    let (events_tx, events_rx) = broadcast::channel::<AgentEvent>(1024);
    let tool_hub = Arc::new(FrontendToolHub::new());
    let status = Arc::new(RwLock::new(ControlStatus {
        state: ControlRunState::Idle,
        queue_len: 0,
        mission_id: None,
    }));
    let current_mission = Arc::new(RwLock::new(None));

    // Channel for agent-initiated mission control commands
    let (mission_cmd_tx, mission_cmd_rx) =
        mpsc::channel::<crate::tools::mission::MissionControlCommand>(64);

    let current_tree = Arc::new(RwLock::new(None));
    let progress = Arc::new(RwLock::new(ExecutionProgress::default()));
    let running_missions = Arc::new(RwLock::new(Vec::new()));
    let mission_search_cache = Arc::new(RwLock::new(HashMap::new()));
    let max_parallel =
        crate::settings::max_parallel_missions_cached_or(config.max_parallel_missions);

    let state = ControlState {
        cmd_tx,
        events_tx: events_tx.clone(),
        tool_hub: Arc::clone(&tool_hub),
        status: Arc::clone(&status),
        current_mission: Arc::clone(&current_mission),
        current_tree: Arc::clone(&current_tree),
        progress: Arc::clone(&progress),
        running_missions: Arc::clone(&running_missions),
        max_parallel,
        mission_store: Arc::clone(&mission_store),
        mission_search_cache,
    };

    // Spawn the main control actor
    tokio::spawn(control_actor_loop(
        config.clone(),
        root_agent,
        mcp,
        workspaces.clone(),
        library.clone(),
        cmd_rx,
        mission_cmd_rx,
        mission_cmd_tx,
        events_tx.clone(),
        events_rx,
        tool_hub,
        status,
        current_mission,
        current_tree,
        progress,
        mission_store,
        secrets,
    ));

    // Recover missions stopped by the previous backend process. Graceful
    // shutdown marks live runners as `interrupted/server_shutdown`; a hard
    // stop can leave task-mode missions as `active`.
    if state.mission_store.is_persistent() {
        let store = Arc::clone(&state.mission_store);
        let tx = events_tx.clone();
        let cmd = state.cmd_tx.clone();
        tokio::spawn(async move {
            recover_server_shutdown_missions(store, tx, cmd).await;
        });
    }

    // Spawn background stale mission cleanup task (if enabled)
    if config.stale_mission_hours > 0 && state.mission_store.is_persistent() {
        tokio::spawn(stale_mission_cleanup_loop(
            Arc::clone(&state.mission_store),
            config.stale_mission_hours,
            state.cmd_tx.clone(),
            events_tx.clone(),
        ));
    }

    // Spawn in-process orphan detector. Every 60 s it asks the control
    // actor for the live running list and marks any mission whose
    // `seconds_since_activity` exceeds the silence threshold as
    // interrupted. This covers two failure modes the existing recovery
    // didn't:
    //  1. mission_runner task died mid-flight (backend alive, codex
    //     orphaned in its container). Boot-time recovery only catches
    //     this across a full restart.
    //  2. codex hung — process alive in `futex_wait_queue` with no
    //     events flowing. Stale-mission cleanup eventually catches this
    //     after `stale_hours` (default 24 h); 10 min is a much closer
    //     match for "agent may be stuck" UX.
    if state.mission_store.is_persistent() {
        tokio::spawn(stuck_mission_watchdog_loop(
            Arc::clone(&state.mission_store),
            state.cmd_tx.clone(),
            events_tx.clone(),
        ));
    }

    // Spawn event logger task (logs all events to SQLite for debugging/replay)
    if state.mission_store.is_persistent() {
        let store = Arc::clone(&state.mission_store);
        let mut event_rx = events_tx.subscribe();
        tokio::spawn(async move {
            let mut pending_text_deltas: HashMap<Uuid, String> = HashMap::new();
            loop {
                match event_rx.recv().await {
                    Ok(event) => {
                        // Extract mission_id from event
                        if let Some(mid) = event.mission_id() {
                            if let Some(content) = synthetic_thought_from_text_delta(
                                &mut pending_text_deltas,
                                mid,
                                &event,
                            ) {
                                let synthetic = AgentEvent::Thinking {
                                    content,
                                    done: true,
                                    mission_id: Some(mid),
                                };
                                if let Err(e) = store.log_event(mid, &synthetic).await {
                                    tracing::warn!("Failed to log synthetic thinking event: {}", e);
                                }
                            }
                            if let Err(e) = store.log_event(mid, &event).await {
                                tracing::warn!("Failed to log event: {}", e);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Event logger lagged by {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            tracing::info!("Event logger task stopped");
        });
    }

    // Spawn automation scheduler task
    if state.mission_store.is_persistent() && config.automations_enabled {
        tokio::spawn(automation_scheduler_loop(
            Arc::clone(&state.mission_store),
            library.clone(),
            state.cmd_tx.clone(),
            workspaces.clone(),
            state.events_tx.clone(),
            telegram_bridge.clone(),
        ));
    } else if state.mission_store.is_persistent() {
        tracing::info!("Automation scheduler disabled by config");
    }

    state
}

async fn recover_server_shutdown_missions(
    mission_store: Arc<dyn MissionStore>,
    events_tx: broadcast::Sender<AgentEvent>,
    cmd_tx: mpsc::Sender<ControlCommand>,
) {
    let mut to_resume = Vec::new();
    let mut seen = HashSet::new();

    match mission_store.get_all_active_missions().await {
        Ok(active_missions) => {
            for mission in active_missions {
                if mission.mission_mode == super::mission_store::MissionMode::Assistant {
                    tracing::debug!(
                        mission_id = %mission.id,
                        "Startup recovery: leaving assistant-mode active mission idle"
                    );
                    continue;
                }

                tracing::warn!(
                    mission_id = %mission.id,
                    title = %mission.title.as_deref().unwrap_or("Untitled"),
                    updated_at = %mission.updated_at,
                    "Startup recovery: active task mission survived restart; marking server_shutdown and auto-resuming"
                );
                if let Err(e) = mission_store
                    .update_mission_status_with_reason(
                        mission.id,
                        MissionStatus::Interrupted,
                        Some("server_shutdown"),
                    )
                    .await
                {
                    tracing::warn!(
                        mission_id = %mission.id,
                        "Startup recovery: failed to mark active mission interrupted: {}",
                        e
                    );
                    continue;
                }

                maybe_schedule_mission_metadata_refresh_for_status(
                    &mission_store,
                    &events_tx,
                    mission.id,
                    MissionStatus::Interrupted,
                );
                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                    mission_id: mission.id,
                    status: MissionStatus::Interrupted,
                    summary: Some(
                        "Interrupted: server restarted while mission was active".to_string(),
                    ),
                });

                if seen.insert(mission.id) {
                    to_resume.push(mission.id);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Startup recovery: failed to check for active missions: {}",
                e
            );
        }
    }

    match mission_store
        .get_recent_server_shutdown_mission_ids(SERVER_SHUTDOWN_AUTO_RESUME_MAX_AGE_HOURS)
        .await
    {
        Ok(mission_ids) => {
            for mission_id in mission_ids {
                if seen.insert(mission_id) {
                    to_resume.push(mission_id);
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Startup recovery: failed to check for server-shutdown missions: {}",
                e
            );
        }
    }

    if to_resume.is_empty() {
        tracing::debug!("Startup recovery: no server-shutdown missions to auto-resume");
        return;
    }

    tracing::warn!(
        count = to_resume.len(),
        "Startup recovery: auto-resuming server-shutdown mission(s)"
    );

    for mission_id in to_resume {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = cmd_tx
            .send(ControlCommand::ResumeMission {
                mission_id,
                clean_workspace: false,
                skip_message: false,
                respond: tx,
            })
            .await
        {
            tracing::warn!(
                mission_id = %mission_id,
                "Startup recovery: failed to enqueue auto-resume: {}",
                e
            );
            continue;
        }

        match rx.await {
            Ok(Ok(_)) => {
                tracing::info!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume queued"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume failed: {}",
                    e
                );
            }
            Err(e) => {
                tracing::warn!(
                    mission_id = %mission_id,
                    "Startup recovery: auto-resume response dropped: {}",
                    e
                );
            }
        }
    }
}

/// Apply the stale-mission safety net once.
///
/// We intentionally do not infer "orphaned" from `MissionStatus::Active` alone here.
/// Missions remain `active` between turns while waiting for the next user message or
/// queued automation, so the periodic cleanup task cannot safely treat "not currently
/// running" as an interruption without spuriously flipping healthy Claude missions to
/// `interrupted`.
async fn cleanup_stale_active_missions_once(
    mission_store: &Arc<dyn MissionStore>,
    stale_hours: u64,
    events_tx: &broadcast::Sender<AgentEvent>,
    cmd_tx: &mpsc::Sender<ControlCommand>,
) {
    match mission_store.get_stale_active_missions(stale_hours).await {
        Ok(stale_missions) => {
            for mission in stale_missions {
                tracing::info!(
                    "Auto-closing stale mission {}: '{}' (inactive since {})",
                    mission.id,
                    mission.title.as_deref().unwrap_or("Untitled"),
                    mission.updated_at
                );

                // Ask the control actor to cancel any in-memory runner
                // for this mission before we overwrite DB status. Without
                // this, a frozen runner (e.g. stuck in `child.wait()` on
                // an orphaned tool subprocess) would keep
                // `running_mission_id` pinned and /api/control/running
                // would keep reporting the mission as "running, stalled"
                // until the daemon restarts. CancelMission is idempotent
                // — it returns "not found" when there is no live runner,
                // which is the common case for stale missions, and we
                // ignore that error.
                let (tx, rx) = oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::CancelMission {
                        mission_id: mission.id,
                        respond: tx,
                    })
                    .await
                    .is_ok()
                {
                    let _ = rx.await;
                }

                if let Err(e) = mission_store
                    .update_mission_status(mission.id, MissionStatus::Completed)
                    .await
                {
                    tracing::warn!("Failed to auto-close stale mission {}: {}", mission.id, e);
                } else {
                    maybe_schedule_mission_metadata_refresh_for_status(
                        mission_store,
                        events_tx,
                        mission.id,
                        MissionStatus::Completed,
                    );
                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                        mission_id: mission.id,
                        status: MissionStatus::Completed,
                        summary: Some(format!(
                            "Auto-closed after {} hours of inactivity",
                            stale_hours
                        )),
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to check for stale missions: {}", e);
        }
    }
}

/// Background task that periodically cleans up stale missions.
/// Periodic watchdog: marks missions interrupted when the runner has
/// stalled for too long, even if the mission row is still `Active`.
///
/// Two cases this catches that the boot-time orphan recovery and the
/// daily stale-mission cleanup miss:
/// 1. mission_runner task died mid-flight (e.g. codex stdio EOF after
///    one of our reconnect attempts). The mission row stays Active
///    forever because nothing emits a terminal status; the codex
///    process can survive in its container namespace.
/// 2. codex itself hung — process alive but `futex_wait_queue` with no
///    events. Observed live on prod after a deploy mid-mission: 70+
///    minutes of silence, dashboard correctly flagged "may be stuck"
///    but no path was forcing termination.
///
/// Threshold is intentionally generous (15 min) so a model in the
/// middle of a slow API turn or a long shell command isn't false-killed.
async fn stuck_mission_watchdog_loop(
    mission_store: Arc<dyn MissionStore>,
    cmd_tx: mpsc::Sender<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    use std::collections::HashSet;

    const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
    /// Silence threshold before declaring a mission stuck. Conservative
    /// so legitimately long codex turns (Lean compiles, CI polls) don't
    /// trigger false positives.
    const STUCK_SECONDS: u64 = 900;

    tracing::info!(
        "Stuck-mission watchdog started: threshold {}s, poll every {}s",
        STUCK_SECONDS,
        CHECK_INTERVAL.as_secs()
    );

    loop {
        tokio::time::sleep(CHECK_INTERVAL).await;

        // Pull the in-memory running list from the actor — same source
        // /api/control/running serves, includes seconds_since_activity.
        let (resp_tx, resp_rx) = oneshot::channel();
        if cmd_tx
            .send(ControlCommand::ListRunning { respond: resp_tx })
            .await
            .is_err()
        {
            tracing::debug!("Stuck-mission watchdog: actor channel closed; exiting");
            return;
        }
        let running_list = match resp_rx.await {
            Ok(list) => list,
            Err(_) => continue,
        };

        // Cross-check against DB: any mission Active in the store but
        // not in `running_list` is an orphan from a runner death.
        let active_missions = match mission_store.get_all_active_missions().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Stuck-mission watchdog: list active failed: {}", e);
                continue;
            }
        };

        let running_ids: HashSet<Uuid> = running_list.iter().map(|info| info.mission_id).collect();

        // Case 1 — actor reports the mission running but stalled past
        // threshold. Cancel via the actor (clean shutdown) and mark
        // the row Interrupted.
        for info in &running_list {
            if info.seconds_since_activity >= STUCK_SECONDS {
                tracing::warn!(
                    "Stuck-mission watchdog: cancelling {} after {}s of inactivity",
                    info.mission_id,
                    info.seconds_since_activity
                );
                let (cancel_tx, cancel_rx) = oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::CancelMission {
                        mission_id: info.mission_id,
                        respond: cancel_tx,
                    })
                    .await
                    .is_ok()
                {
                    let _ = cancel_rx.await;
                }
                if let Err(e) = mission_store
                    .update_mission_status_with_reason(
                        info.mission_id,
                        MissionStatus::Interrupted,
                        Some("watchdog_stalled"),
                    )
                    .await
                {
                    tracing::warn!(
                        "Stuck-mission watchdog: status update failed for {}: {}",
                        info.mission_id,
                        e
                    );
                    continue;
                }
                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                    mission_id: info.mission_id,
                    status: MissionStatus::Interrupted,
                    summary: Some(format!(
                        "Interrupted: no agent activity for {}s (>{}s threshold)",
                        info.seconds_since_activity, STUCK_SECONDS
                    )),
                });
            }
        }

        // Case 2 — Active in DB, not in actor's running list at all.
        // This is the "mission_runner died, row never finalized" path.
        for mission in &active_missions {
            if running_ids.contains(&mission.id) {
                continue;
            }
            tracing::warn!(
                "Stuck-mission watchdog: orphan {} (no live runner); marking interrupted",
                mission.id
            );
            if let Err(e) = mission_store
                .update_mission_status_with_reason(
                    mission.id,
                    MissionStatus::Interrupted,
                    Some("orphan_no_runner"),
                )
                .await
            {
                tracing::warn!(
                    "Stuck-mission watchdog: status update failed for {}: {}",
                    mission.id,
                    e
                );
                continue;
            }
            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                mission_id: mission.id,
                status: MissionStatus::Interrupted,
                summary: Some(
                    "Interrupted: mission runner exited without reporting a terminal status"
                        .to_string(),
                ),
            });
        }
    }
}

async fn stale_mission_cleanup_loop(
    mission_store: Arc<dyn MissionStore>,
    stale_hours: u64,
    cmd_tx: mpsc::Sender<ControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
) {
    // Check every 5 minutes; the stale timeout remains a safety net for missions that
    // never receive an explicit terminal status.
    let check_interval = std::time::Duration::from_secs(300);

    tracing::info!(
        "Mission cleanup task started: stale timeout {} hours",
        stale_hours
    );

    loop {
        tokio::time::sleep(check_interval).await;
        cleanup_stale_active_missions_once(&mission_store, stale_hours, &events_tx, &cmd_tx).await;
    }
}

/// Resolve an IANA timezone string to a chrono::FixedOffset at a given UTC instant.
///
/// Falls back to UTC if the timezone is unknown.  We use a simple lookup table for
/// common timezones to avoid pulling in chrono-tz (heavy dependency).  The offset is
/// evaluated at `now_utc` to account for DST — though the lookup table doesn't model
/// DST transitions, the most common use-case (Europe/Paris, America/New_York, etc.)
/// is close enough for a 5-second poll cadence.
/// Map a timezone string to a `chrono_tz::Tz`, handling common abbreviations
/// and IANA names.  Returns `None` for fixed-offset strings like "+02:00".
fn resolve_tz(tz: &str) -> Option<chrono_tz::Tz> {
    // Fixed-offset strings ("+02:00", "-05:00") are not IANA timezones.
    if tz.starts_with('+') || tz.starts_with('-') {
        return None;
    }

    // Handle common abbreviations that chrono-tz doesn't know about.
    let canonical = match tz {
        "UTC" | "GMT" => "Etc/UTC",
        "EST" => "America/New_York",
        "CST" => "America/Chicago",
        "MST" => "America/Denver",
        "PST" => "America/Los_Angeles",
        "CET" => "Europe/Paris",
        "JST" => "Asia/Tokyo",
        "AEST" => "Australia/Sydney",
        other => other,
    };

    match canonical.parse::<chrono_tz::Tz>() {
        Ok(timezone) => Some(timezone),
        Err(_) => {
            tracing::warn!(timezone = %tz, "Unknown timezone, rejecting");
            None
        }
    }
}

fn resolve_tz_offset(tz: &str, now_utc: chrono::DateTime<chrono::Utc>) -> chrono::FixedOffset {
    // Try to parse as a fixed offset first (e.g. "+02:00", "-05:00").
    if let Ok(fo) = tz.parse::<chrono::FixedOffset>() {
        return fo;
    }

    use chrono::Offset;
    match resolve_tz(tz) {
        Some(timezone) => {
            let local_dt = now_utc.with_timezone(&timezone);
            local_dt.offset().fix()
        }
        None => chrono::FixedOffset::east_opt(0).unwrap(),
    }
}

/// Background task that checks for automations and triggers them at their intervals.
async fn automation_scheduler_loop(
    mission_store: Arc<dyn MissionStore>,
    library: SharedLibrary,
    cmd_tx: mpsc::Sender<ControlCommand>,
    workspaces: workspace::SharedWorkspaceStore,
    events_tx: broadcast::Sender<AgentEvent>,
    telegram_bridge: Option<super::telegram::SharedTelegramBridge>,
) {
    use super::automation_variables::{substitute_variables, SubstitutionContext};
    use super::mission_store::{AutomationExecution, CommandSource, ExecutionStatus, TriggerType};

    // Check every 5 seconds for automations that need to run
    let check_interval = std::time::Duration::from_secs(5);

    tracing::info!(
        telegram_bridge_available = telegram_bridge.is_some(),
        "Automation scheduler task started"
    );

    let mut logged_unsupported = false;
    let mut tick_count: u64 = 0;

    loop {
        tokio::time::sleep(check_interval).await;
        tick_count += 1;

        // Every ~60 seconds (12 ticks × 5s), run housekeeping sweeps.
        if tick_count.is_multiple_of(12) {
            // Timeout stale WaitingExternal workflows (older than 30 minutes).
            match mission_store
                .timeout_stale_telegram_workflows(30 * 60)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!("Timed out {} stale WaitingExternal Telegram workflows", n);
                }
                Err(e) => {
                    tracing::warn!("Failed to timeout stale Telegram workflows: {}", e);
                }
                _ => {}
            }

            // Recover stale 'sending' scheduled messages (stuck >5 min after crash).
            match mission_store
                .recover_stale_sending_scheduled_messages(5 * 60)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        "Recovered {} stale 'sending' scheduled messages back to 'pending'",
                        n
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to recover stale sending messages: {}", e);
                }
                _ => {}
            }

            // Clean up old webhook dedup entries (older than 15 minutes).
            match mission_store.cleanup_webhook_dedup(15 * 60).await {
                Ok(n) if n > 0 => {
                    tracing::debug!("Cleaned up {} expired webhook dedup entries", n);
                }
                Err(e) => {
                    tracing::warn!("Failed to cleanup webhook dedup entries: {}", e);
                }
                _ => {}
            }
        }

        let automations = match mission_store.list_active_automations().await {
            Ok(automations) => automations,
            Err(e) => {
                if !logged_unsupported {
                    tracing::warn!("Automation scheduler disabled: {}", e);
                    logged_unsupported = true;
                }
                continue;
            }
        };

        for automation in automations {
            // Only trigger interval-based and cron-based automations.
            // Webhooks are triggered via HTTP, agent_finished via turn completion,
            // Telegram via the Telegram bridge.
            enum ScheduleKind {
                Interval(u64),
                Cron {
                    expression: String,
                    timezone: String,
                },
            }
            let schedule = match &automation.trigger {
                TriggerType::Interval { seconds } => ScheduleKind::Interval(*seconds),
                TriggerType::Cron {
                    expression,
                    timezone,
                } => ScheduleKind::Cron {
                    expression: expression.clone(),
                    timezone: timezone.clone(),
                },
                TriggerType::Webhook { .. } => continue,
                TriggerType::AgentFinished => continue,
                TriggerType::Telegram { .. } => continue,
            };

            let mission = match mission_store.get_mission(automation.mission_id).await {
                Ok(Some(mission)) => mission,
                Ok(None) => {
                    tracing::debug!(
                        "Automation {} references missing mission {}",
                        automation.id,
                        automation.mission_id
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load mission {} for automation {}: {}",
                        automation.mission_id,
                        automation.id,
                        e
                    );
                    continue;
                }
            };

            let consecutive_failures =
                consecutive_failure_count_for_automation(&mission_store, &automation).await;

            if stop_policy_matches_status(
                &automation.stop_policy,
                mission.status,
                consecutive_failures,
            )
            .await
            {
                tracing::info!(
                    "Disabling automation {} due to stop policy {:?} (mission {} status {:?})",
                    automation.id,
                    automation.stop_policy,
                    mission.id,
                    mission.status
                );
                let mut updated = automation.clone();
                updated.active = false;
                if let Err(e) = mission_store.update_automation(updated).await {
                    tracing::warn!(
                        "Failed to disable automation {} after stop policy match: {}",
                        automation.id,
                        e
                    );
                }
                continue;
            }

            // Check if it's time to trigger based on schedule type.
            let should_trigger = match &schedule {
                ScheduleKind::Interval(interval_seconds) => {
                    if let Some(ref last_triggered) = automation.last_triggered_at {
                        match chrono::DateTime::parse_from_rfc3339(last_triggered) {
                            Ok(last_time) => {
                                let elapsed = chrono::Utc::now()
                                    .signed_duration_since(last_time.with_timezone(&chrono::Utc));
                                elapsed.num_seconds() >= *interval_seconds as i64
                            }
                            Err(_) => true,
                        }
                    } else {
                        true // Never triggered before
                    }
                }
                ScheduleKind::Cron {
                    expression,
                    timezone,
                } => {
                    match croner::Cron::new(expression).parse() {
                        Ok(cron) => {
                            // Determine "now" in the configured timezone.
                            let now_utc = chrono::Utc::now();

                            // If we've never triggered (start_immediately=true sets
                            // last_triggered_at to None), fire right away.
                            let reference = if let Some(ref lt) = automation.last_triggered_at {
                                match chrono::DateTime::parse_from_rfc3339(lt) {
                                    Ok(t) => t.with_timezone(&chrono::Utc),
                                    Err(_) => now_utc - chrono::Duration::seconds(10),
                                }
                            } else {
                                // Never triggered → fire immediately on next tick.
                                // Using a very large lookback ensures the next cron
                                // occurrence after this reference is in the past.
                                now_utc - chrono::Duration::days(366)
                            };

                            // Find the next occurrence after the last trigger.
                            // If that occurrence is <= now, it's time to fire.
                            // Use real timezone (not a FixedOffset snapshot) so DST
                            // transitions are evaluated correctly by croner.
                            if let Some(tz) = resolve_tz(timezone) {
                                let ref_with_tz = reference.with_timezone(&tz);
                                match cron.find_next_occurrence(&ref_with_tz, false) {
                                    Ok(next) => {
                                        let next_utc = next.with_timezone(&chrono::Utc);
                                        next_utc <= now_utc
                                    }
                                    Err(_) => false,
                                }
                            } else {
                                // Fixed-offset timezone string (e.g. "+02:00")
                                let tz_offset = resolve_tz_offset(timezone, now_utc);
                                let ref_with_tz = reference.with_timezone(&tz_offset);
                                match cron.find_next_occurrence(&ref_with_tz, false) {
                                    Ok(next) => {
                                        let next_utc = next.with_timezone(&chrono::Utc);
                                        next_utc <= now_utc
                                    }
                                    Err(_) => false,
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                automation_id = %automation.id,
                                expression = %expression,
                                "Invalid cron expression, skipping: {}", e
                            );
                            false
                        }
                    }
                }
            };

            if !should_trigger {
                continue;
            }

            // Check if the mission is currently busy (has a running task or queued messages)
            let is_busy = {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cmd_tx
                    .send(ControlCommand::ListRunning { respond: tx })
                    .await
                    .is_err()
                {
                    tracing::warn!("Failed to send ListRunning command for automation busy check");
                    continue;
                }
                match rx.await {
                    Ok(running) => running.iter().any(|r| {
                        r.mission_id == mission.id
                            && (r.queue_len > 0
                                || matches!(r.state.as_str(), "running" | "waiting_for_tool"))
                    }),
                    Err(_) => {
                        tracing::warn!(
                            "Failed to receive ListRunning response for automation busy check"
                        );
                        continue;
                    }
                }
            };

            if is_busy {
                tracing::debug!(
                    "Mission {} is busy, skipping automation trigger",
                    mission.id
                );
                continue;
            }

            // Get workspace for reading local files
            let workspace = workspaces.get(mission.workspace_id).await;

            // Fetch the command content based on the command source
            let command_content = match &automation.command_source {
                CommandSource::Library { name } => {
                    if let Some(lib) = library.read().await.as_ref() {
                        match lib.get_command(name).await {
                            Ok(command) => automation_library_command_body(&command.content),
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to fetch command '{}' for automation {}: {}",
                                    name,
                                    automation.id,
                                    e
                                );
                                continue;
                            }
                        }
                    } else {
                        tracing::debug!("Library not initialized, skipping automation trigger");
                        continue;
                    }
                }
                CommandSource::LocalFile { path } => {
                    // Read file from mission workspace
                    let file_path = if let Some(ws) = workspace.as_ref() {
                        ws.path.join(path)
                    } else {
                        tracing::warn!(
                            "Workspace {} not found for automation {}",
                            mission.workspace_id,
                            automation.id
                        );
                        continue;
                    };

                    match tokio::fs::read_to_string(&file_path).await {
                        Ok(content) => content,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to read file '{}' for automation {}: {}",
                                file_path.display(),
                                automation.id,
                                e
                            );
                            continue;
                        }
                    }
                }
                CommandSource::Inline { content } => content.clone(),
            };

            // Build substitution context for variable replacement
            let mut context = SubstitutionContext::new(mission.id);
            if let Some(ref title) = mission.title {
                context = context.with_mission_name(title.clone());
            }
            if let Some(ws) = workspace.as_ref() {
                context = context.with_working_directory(ws.path.to_string_lossy().to_string());
            }
            context = context.with_custom_variables(automation.variables.clone());

            // Apply variable substitution
            let substituted_content = substitute_variables(&command_content, &context);

            // Create execution record before execution
            let execution_id = Uuid::new_v4();
            let execution = AutomationExecution {
                id: execution_id,
                automation_id: automation.id,
                mission_id: mission.id,
                triggered_at: mission_store::now_string(),
                trigger_source: "interval".to_string(),
                status: ExecutionStatus::Pending,
                webhook_payload: None,
                variables_used: automation.variables.clone(),
                completed_at: None,
                error: None,
                retry_count: 0,
            };

            let execution = match mission_store.create_automation_execution(execution).await {
                Ok(exec) => exec,
                Err(e) => {
                    tracing::warn!(
                        "Failed to create execution record for automation {}: {}",
                        automation.id,
                        e
                    );
                    continue;
                }
            };

            tracing::info!(
                "Triggering automation {} (execution {}) for mission {}",
                automation.id,
                execution_id,
                mission.id
            );

            // Update execution status to Running
            let mut exec = execution.clone();
            exec.status = ExecutionStatus::Running;
            if let Err(e) = mission_store.update_automation_execution(exec).await {
                tracing::warn!(
                    "Failed to update execution status to running for {}: {}",
                    execution_id,
                    e
                );
            }

            // Send the message to the mission with retry logic
            let mut retry_attempt = 0;
            let max_retries = automation.retry_config.max_retries;
            let base_delay = automation.retry_config.retry_delay_seconds;
            let backoff_multiplier = automation.retry_config.backoff_multiplier;

            loop {
                let message_id = Uuid::new_v4();
                let (respond_tx, _respond_rx) = tokio::sync::oneshot::channel();

                let send_result = cmd_tx
                    .send(ControlCommand::UserMessage {
                        id: message_id,
                        content: substituted_content.clone(),
                        agent: None,
                        target_mission_id: Some(mission.id),
                        respond: respond_tx,
                    })
                    .await;

                match send_result {
                    Ok(_) => {
                        // Message queued successfully – keep execution in Running
                        // status. The actual success/failure will be determined
                        // when the agent finishes processing and
                        // complete_running_executions_for_mission is called.
                        let mut exec = execution.clone();
                        exec.retry_count = retry_attempt;
                        if let Err(e) = mission_store.update_automation_execution(exec).await {
                            tracing::warn!(
                                "Failed to update execution retry count for {}: {}",
                                execution_id,
                                e
                            );
                        }

                        // Update last triggered time
                        if let Err(e) = mission_store
                            .update_automation_last_triggered(automation.id)
                            .await
                        {
                            tracing::warn!(
                                "Failed to update automation last triggered time: {}",
                                e
                            );
                        }

                        // Route response to Telegram if this mission has an
                        // associated Telegram chat (proactive messaging).
                        if let Some(ref bridge) = telegram_bridge {
                            let mission_id = mission.id;
                            let store = Arc::clone(&mission_store);
                            let bridge = Arc::clone(bridge);
                            let tg_events_rx = events_tx.subscribe();
                            tokio::spawn(async move {
                                // Look up the Telegram chat for this mission
                                let chat_mapping = store
                                    .get_telegram_chat_mission_by_mission_id(mission_id)
                                    .await;
                                if let Ok(Some(mapping)) = chat_mapping {
                                    // Find the channel context to get the bot token
                                    if let Some(ctx) =
                                        bridge.get_channel_context(mapping.channel_id).await
                                    {
                                        tracing::info!(
                                            "Routing automation response for mission {} to Telegram chat {}",
                                            mission_id,
                                            mapping.chat_id
                                        );
                                        if let Err(e) = super::telegram::stream_response(
                                            tg_events_rx,
                                            bridge.http(),
                                            &ctx.channel.bot_token,
                                            mapping.chat_id,
                                            0, // no reply_to for proactive messages
                                            None,
                                            mission_id,
                                            Some(Arc::clone(&bridge)),
                                            Some(mapping.channel_id),
                                            Some(Arc::clone(&store)),
                                        )
                                        .await
                                        {
                                            tracing::warn!(
                                                "Failed to stream automation response to Telegram: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            });
                        }

                        break;
                    }
                    Err(e) => {
                        if retry_attempt < max_retries {
                            // Calculate exponential backoff delay
                            let delay_seconds =
                                base_delay as f64 * backoff_multiplier.powi(retry_attempt as i32);

                            tracing::warn!(
                                "Failed to send automation message (attempt {}/{}): {}. Retrying in {:.1}s",
                                retry_attempt + 1,
                                max_retries + 1,
                                e,
                                delay_seconds
                            );

                            retry_attempt += 1;

                            // Wait before retry
                            tokio::time::sleep(std::time::Duration::from_secs_f64(delay_seconds))
                                .await;
                        } else {
                            // Max retries exceeded - mark as failed
                            tracing::error!(
                                "Failed to send automation message after {} attempts: {}",
                                max_retries + 1,
                                e
                            );

                            let mut exec = execution.clone();
                            exec.status = ExecutionStatus::Failed;
                            exec.completed_at = Some(mission_store::now_string());
                            exec.error =
                                Some(format!("Failed after {} retries: {}", max_retries + 1, e));
                            exec.retry_count = retry_attempt;

                            if let Err(e) = mission_store.update_automation_execution(exec).await {
                                tracing::warn!(
                                    "Failed to update execution status to failed for {}: {}",
                                    execution_id,
                                    e
                                );
                            }

                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Keep automation library command execution consistent with `/command` usage:
/// frontmatter is metadata and should never be injected into model prompts.
fn automation_library_command_body(command_content: &str) -> String {
    let (_frontmatter, body) = crate::library::types::parse_frontmatter(command_content);
    body.trim().to_string()
}

/// Resolve the command content for a single automation, applying variable
/// substitution.  Returns `None` if the command cannot be resolved (e.g.
/// library unavailable, file not found).
async fn resolve_automation_command(
    automation: &mission_store::Automation,
    mission_id: Uuid,
    state: &Arc<AppState>,
    store: &Arc<dyn MissionStore>,
) -> Option<String> {
    use super::automation_variables::{substitute_variables, SubstitutionContext};
    use super::mission_store::CommandSource;

    let mission = store.get_mission(mission_id).await.ok()??;
    let workspace = state.workspaces.get(mission.workspace_id).await;

    let command_content = match &automation.command_source {
        CommandSource::Library { name } => {
            let lib = state.library.read().await;
            let lib = lib.as_ref()?;
            lib.get_command(name)
                .await
                .ok()
                .map(|c| automation_library_command_body(&c.content))?
        }
        CommandSource::LocalFile { path } => {
            let ws = workspace.as_ref()?;
            tokio::fs::read_to_string(ws.path.join(path)).await.ok()?
        }
        CommandSource::Inline { content } => content.clone(),
    };

    let mut context = SubstitutionContext::new(mission.id);
    if let Some(ref title) = mission.title {
        context = context.with_mission_name(title.clone());
    }
    if let Some(ws) = workspace.as_ref() {
        context = context.with_working_directory(ws.path.to_string_lossy().to_string());
    }
    context = context.with_custom_variables(automation.variables.clone());

    Some(substitute_variables(&command_content, &context))
}

#[derive(Debug, Clone)]
struct RoutedAutomationMessage {
    content: String,
    target_mission_id: Uuid,
}

fn enqueue_agent_finished_messages(
    queue: &mut VecDeque<(Uuid, String, Option<String>, Option<Uuid>)>,
    messages: Vec<RoutedAutomationMessage>,
) {
    for message in messages {
        queue.push_back((
            Uuid::new_v4(),
            message.content,
            None,
            Some(message.target_mission_id),
        ));
    }
}

fn queue_has_pending_target_mission(
    queue: &VecDeque<(Uuid, String, Option<String>, Option<Uuid>)>,
    mission_id: Uuid,
) -> bool {
    queue
        .iter()
        .any(|(_id, _msg, _agent, target_mid)| *target_mid == Some(mission_id))
}

fn mission_status_for_terminal_reason(
    reason: TerminalReason,
    complete_turn_without_follow_up: bool,
) -> Option<(MissionStatus, &'static str)> {
    match reason {
        TerminalReason::TurnComplete if complete_turn_without_follow_up => {
            Some((MissionStatus::Completed, "completed"))
        }
        TerminalReason::TurnComplete => None,
        TerminalReason::Completed => Some((MissionStatus::Completed, "completed")),
        TerminalReason::Cancelled => Some((MissionStatus::Interrupted, "cancelled")),
        TerminalReason::ServerShutdown => Some((MissionStatus::Interrupted, "server_shutdown")),
        TerminalReason::MaxIterations => Some((MissionStatus::Blocked, "max_iterations")),
        TerminalReason::LlmError => Some((MissionStatus::Failed, "llm_error")),
        TerminalReason::Stalled => Some((MissionStatus::Failed, "stalled")),
        TerminalReason::InfiniteLoop => Some((MissionStatus::Failed, "infinite_loop")),
        TerminalReason::RateLimited => Some((MissionStatus::Failed, "rate_limited")),
        TerminalReason::CapacityLimited => Some((MissionStatus::Failed, "capacity_limited")),
        TerminalReason::AuthError => Some((MissionStatus::Failed, "auth_error")),
    }
}

fn mission_status_summary_for_terminal_reason(reason: TerminalReason) -> Option<String> {
    match reason {
        TerminalReason::TurnComplete | TerminalReason::Completed => None,
        TerminalReason::MaxIterations => Some("Reached iteration limit".to_string()),
        TerminalReason::Cancelled => Some("Cancelled by user".to_string()),
        TerminalReason::ServerShutdown => {
            Some("Paused for server restart — click Resume to continue".to_string())
        }
        TerminalReason::Stalled => Some("No progress detected".to_string()),
        TerminalReason::InfiniteLoop => Some("Detected repetitive behavior".to_string()),
        TerminalReason::LlmError => Some("Model error".to_string()),
        TerminalReason::RateLimited => Some("Provider rate limited".to_string()),
        TerminalReason::CapacityLimited => Some("Provider capacity limit reached".to_string()),
        TerminalReason::AuthError => Some("Authentication failed".to_string()),
    }
}

fn parse_goal_objective(message: &str) -> Option<String> {
    message
        .trim_start()
        .strip_prefix("/goal ")
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
        .map(ToString::to_string)
}

/// If the turn ended with `LlmError` or `AuthError` but the agent produced
/// substantive output, downgrade the reason to `TurnComplete` so the mission
/// stays active and can be picked up by the next automation cycle or user
/// message.  This prevents transient backend errors (e.g. Codex "Failed to
/// shutdown rollout recorder", or Claude Code exiting with code 1 after a
/// successful turn that happens to contain an auth-error string in its
/// output) from killing missions that actually completed their work.
fn maybe_recover_soft_llm_error(result: &mut crate::agents::AgentResult) {
    let is_recoverable = matches!(
        result.terminal_reason,
        Some(TerminalReason::LlmError) | Some(TerminalReason::AuthError)
    );
    if !is_recoverable {
        return;
    }
    // Claude Code transport failures (startup timeout, incomplete turn, etc.)
    // carry a structured `claudecode_transport_failure` marker. These are
    // never a successful turn — treating them as TurnComplete lets the mission
    // re-enter an automation loop where every retry fake-succeeds. Keep the
    // failure classification so the mission is surfaced as failed.
    if result
        .data
        .as_ref()
        .and_then(|v| v.get("claudecode_transport_failure"))
        .is_some()
    {
        return;
    }
    let output = result.output.trim();
    // Only recover when we have real content — not just an error message.
    // Heuristic: at least 20 chars and doesn't look like a bare error.
    if output.len() >= 20
        && !output.starts_with("Codex produced no output")
        && !output.starts_with("Codex CLI produced no JSON")
        && !output.starts_with("Codex CLI exited before completing the turn")
        && !output.starts_with("No response from")
        && !output.starts_with("Claude Code error:")
        && !output.starts_with("Claude Code produced no")
        && !output.starts_with("Claude Code emitted malformed")
        && !output.starts_with("Claude Code ended before startup")
        && !output.starts_with("Claude Code exited without")
        && !output.starts_with("Claude Code stopped producing output")
        && !output.starts_with("No Claude Code credentials detected")
        && !is_bare_llm_error_output(output)
    {
        tracing::info!(
            output_len = output.len(),
            reason = ?result.terminal_reason,
            "Recovering from soft error: agent produced valid output, upgrading to TurnComplete"
        );
        result.success = true;
        result.terminal_reason = Some(TerminalReason::TurnComplete);
    }
}

fn is_bare_llm_error_output(output: &str) -> bool {
    if looks_like_structured_provider_error(output) {
        return true;
    }
    if super::mission_runner::is_provider_payload_error(output) {
        return true;
    }

    let normalized = output
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | '!' | '"' | '\''))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    if normalized.is_empty() {
        return false;
    }

    matches!(
        normalized.as_str(),
        "internal server error"
            | "invalid authentication credentials"
            | "no claude code credentials detected"
            | "unknown error"
            | "service unavailable"
            | "bad gateway"
            | "gateway timeout"
            | "request timeout"
            | "upstream error"
            | "model error"
    ) || normalized.starts_with("api error:")
        || normalized.starts_with("anthropic api error:")
        || normalized.starts_with("claude code error:")
        // Claude Code's canonical auth-failure surface: the CLI prints
        // `Failed to authenticate. API Error: 401 ...` when Anthropic
        // rejects the request mid-turn. Without this pattern the
        // short auth-error string slipped past
        // `maybe_recover_soft_llm_error` and got fake-promoted to
        // TurnComplete, hiding rotation exhaustion from the UI.
        || normalized.starts_with("failed to authenticate")
        // Any short output whose only substantive content is an auth
        // HTTP status from Anthropic/OpenAI — catches phrasings like
        // `<some prefix>. API Error: 401 ...` without needing to
        // enumerate the prefix.
        || (normalized.len() < 200
            && (normalized.contains("api error: 401")
                || normalized.contains("api error: 403")
                || normalized.contains("api error: 407")))
}

fn looks_like_structured_provider_error(output: &str) -> bool {
    let trimmed = output.trim();
    if !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();
    let has_error_shape = lower.contains("\"detail\"")
        || lower.contains("\"error\"")
        || lower.contains("\"message\"")
        || lower.contains("\"type\"");
    if !has_error_shape {
        return false;
    }

    lower.contains("invalid_request_error")
        || lower.contains("model is not supported")
        || lower.contains("not supported when using codex")
        || lower.contains("does not exist or you do not have access")
        || lower.contains("invalid authentication credentials")
        || lower.contains("rate limit")
        || lower.contains("capacity")
        || lower.contains("service unavailable")
        || lower.contains("internal server error")
}

async fn maybe_finalize_terminal_mission(
    mission_store: &Arc<dyn MissionStore>,
    events_tx: &tokio::sync::broadcast::Sender<AgentEvent>,
    mission_id: Uuid,
    terminal_reason: Option<TerminalReason>,
    complete_turn_without_follow_up: bool,
    log_context: &str,
) {
    let Some(reason) = terminal_reason else {
        return;
    };
    let Some((new_status, terminal_reason_str)) =
        mission_status_for_terminal_reason(reason, complete_turn_without_follow_up)
    else {
        tracing::debug!(
            mission_id = %mission_id,
            reason = ?reason,
            context = log_context,
            "Skipping mission finalization for non-terminal turn state"
        );
        return;
    };

    match mission_store.get_mission(mission_id).await {
        Ok(Some(mission)) => {
            if mission.status == MissionStatus::Interrupted {
                tracing::debug!(
                    mission_id = %mission_id,
                    reason = ?reason,
                    context = log_context,
                    "Skipping mission finalization because mission is already interrupted"
                );
                return;
            }

            if !matches!(
                mission.status,
                MissionStatus::Active | MissionStatus::Pending
            ) {
                tracing::debug!(
                    mission_id = %mission_id,
                    status = ?mission.status,
                    context = log_context,
                    "Skipping mission finalization because mission already has terminal status"
                );
                return;
            }

            if new_status == MissionStatus::Completed
                && mission_has_active_automation(mission_store, mission_id).await
            {
                tracing::info!(
                    mission_id = %mission_id,
                    context = log_context,
                    "Skipping mission completion because active automations are enabled"
                );
                return;
            }

            // Assistant missions (e.g. Telegram-linked) should stay active
            // after each reply — they are long-lived by design.
            if new_status == MissionStatus::Completed
                && mission.mission_mode == super::mission_store::MissionMode::Assistant
            {
                tracing::debug!(
                    mission_id = %mission_id,
                    context = log_context,
                    "Skipping mission completion for assistant-mode mission"
                );
                return;
            }

            tracing::info!(
                mission_id = %mission_id,
                status = ?new_status,
                reason = ?reason,
                context = log_context,
                "Finalizing mission after terminal turn"
            );
            if let Err(e) = mission_store
                .update_mission_status_with_reason(
                    mission_id,
                    new_status,
                    Some(terminal_reason_str),
                )
                .await
            {
                tracing::warn!(
                    mission_id = %mission_id,
                    context = log_context,
                    "Failed to finalize mission after terminal turn: {}",
                    e
                );
            } else {
                maybe_schedule_mission_metadata_refresh_for_status(
                    mission_store,
                    events_tx,
                    mission_id,
                    new_status,
                );
                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                    mission_id,
                    status: new_status,
                    summary: mission_status_summary_for_terminal_reason(reason),
                });
            }
        }
        Ok(None) => {
            tracing::warn!(
                mission_id = %mission_id,
                context = log_context,
                "Mission not found for terminal finalization"
            );
        }
        Err(e) => {
            tracing::warn!(
                mission_id = %mission_id,
                context = log_context,
                "Failed to load mission for terminal finalization: {}",
                e
            );
        }
    }
}

fn next_session_id_from_automation_variables(
    automation: &super::mission_store::Automation,
) -> Option<Uuid> {
    let raw = automation.variables.get("nextSessionId")?.trim();
    if raw.is_empty() {
        return None;
    }
    Uuid::parse_str(raw).ok()
}

fn resolve_agent_finished_target_mission(
    source_mission_id: Uuid,
    source_mission_status: MissionStatus,
    automation: &super::mission_store::Automation,
) -> Uuid {
    if source_mission_status == MissionStatus::Completed
        && automation.fresh_session == mission_store::FreshSession::Switch
    {
        if let Some(next_id) = next_session_id_from_automation_variables(automation) {
            return next_id;
        }

        tracing::warn!(
            "Automation {} is in switch mode but has no valid nextSessionId variable; falling back to mission {}",
            automation.id,
            source_mission_id
        );
    }

    source_mission_id
}

async fn agent_finished_automation_messages(
    mission_store: &Arc<dyn MissionStore>,
    mission_id: Uuid,
    library: &SharedLibrary,
    workspaces: &workspace::SharedWorkspaceStore,
) -> Vec<RoutedAutomationMessage> {
    use super::automation_variables::{substitute_variables, SubstitutionContext};
    use super::mission_store::{AutomationExecution, CommandSource, ExecutionStatus, TriggerType};

    let automations = match mission_store.get_mission_automations(mission_id).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(
                "Failed to load automations for mission {} (agent_finished hook): {}",
                mission_id,
                e
            );
            return Vec::new();
        }
    };

    let mut active: Vec<super::mission_store::Automation> = automations
        .into_iter()
        .filter(|a| a.active && matches!(a.trigger, TriggerType::AgentFinished))
        .collect();

    if active.is_empty() {
        return Vec::new();
    }

    let mission = match mission_store.get_mission(mission_id).await {
        Ok(Some(m)) => m,
        Ok(None) => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                "Failed to load mission {} for agent_finished automations: {}",
                mission_id,
                e
            );
            return Vec::new();
        }
    };

    let mut eligible = Vec::with_capacity(active.len());
    for automation in active {
        let consecutive_failures =
            consecutive_failure_count_for_automation(mission_store, &automation).await;

        if stop_policy_matches_status(
            &automation.stop_policy,
            mission.status,
            consecutive_failures,
        )
        .await
        {
            tracing::info!(
                "Disabling agent_finished automation {} due to stop policy {:?} (mission {} status {:?})",
                automation.id,
                automation.stop_policy,
                mission.id,
                mission.status
            );
            let mut updated = automation.clone();
            updated.active = false;
            if let Err(e) = mission_store.update_automation(updated).await {
                tracing::warn!(
                    "Failed to disable automation {} after stop policy match: {}",
                    automation.id,
                    e
                );
            }
            continue;
        }
        eligible.push(automation);
    }
    active = eligible;

    if active.is_empty() {
        return Vec::new();
    }

    // Stable ordering to avoid surprising changes in multi-automation setups.
    active.sort_by_key(|a| a.created_at.clone());

    let workspace = workspaces.get(mission.workspace_id).await;

    let mut out = Vec::with_capacity(active.len());

    for automation in active {
        // Fetch the command content based on the command source
        let command_content = match &automation.command_source {
            CommandSource::Library { name } => {
                if let Some(lib) = library.read().await.as_ref() {
                    match lib.get_command(name).await {
                        Ok(command) => automation_library_command_body(&command.content),
                        Err(e) => {
                            tracing::warn!(
                                "Failed to fetch command '{}' for automation {}: {}",
                                name,
                                automation.id,
                                e
                            );
                            continue;
                        }
                    }
                } else {
                    tracing::debug!(
                        "Library not initialized, skipping agent_finished automation trigger"
                    );
                    continue;
                }
            }
            CommandSource::LocalFile { path } => {
                let file_path = if let Some(ws) = workspace.as_ref() {
                    ws.path.join(path)
                } else {
                    tracing::warn!(
                        "Workspace {} not found for automation {}",
                        mission.workspace_id,
                        automation.id
                    );
                    continue;
                };
                match tokio::fs::read_to_string(&file_path).await {
                    Ok(content) => content,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to read file '{}' for automation {}: {}",
                            file_path.display(),
                            automation.id,
                            e
                        );
                        continue;
                    }
                }
            }
            CommandSource::Inline { content } => content.clone(),
        };

        // Build substitution context for variable replacement
        let mut context = SubstitutionContext::new(mission.id);
        if let Some(ref title) = mission.title {
            context = context.with_mission_name(title.clone());
        }
        if let Some(ws) = workspace.as_ref() {
            context = context.with_working_directory(ws.path.to_string_lossy().to_string());
        }
        context = context.with_custom_variables(automation.variables.clone());

        let substituted_content = substitute_variables(&command_content, &context);

        let target_mission_id =
            resolve_agent_finished_target_mission(mission.id, mission.status, &automation);

        // Create an execution record in Running status – it will be
        // completed by complete_running_executions_for_mission when the
        // target agent finishes processing.
        let execution_id = Uuid::new_v4();
        let execution = AutomationExecution {
            id: execution_id,
            automation_id: automation.id,
            mission_id: target_mission_id,
            triggered_at: mission_store::now_string(),
            trigger_source: "agent_finished".to_string(),
            status: ExecutionStatus::Running,
            webhook_payload: None,
            variables_used: automation.variables.clone(),
            completed_at: None,
            error: None,
            retry_count: 0,
        };

        if mission_store
            .create_automation_execution(execution)
            .await
            .is_ok()
        {
            // Best-effort: update last_triggered_at for visibility in UI.
            if let Err(e) = mission_store
                .update_automation_last_triggered(automation.id)
                .await
            {
                tracing::warn!("Failed to update automation last triggered time: {}", e);
            }
        } else {
            // If we can't record execution, still trigger the message.
            tracing::warn!(
                "Failed to create execution record for agent_finished automation {}",
                automation.id
            );
        }

        tracing::info!(
            "Triggering agent_finished automation {} (execution {}) from mission {} to mission {}",
            automation.id,
            execution_id,
            mission.id,
            target_mission_id
        );

        out.push(RoutedAutomationMessage {
            content: substituted_content,
            target_mission_id,
        });
    }

    out
}

#[allow(
    clippy::too_many_arguments,
    clippy::collapsible_match,
    clippy::collapsible_else_if
)]
async fn control_actor_loop(
    config: Config,
    root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    mut cmd_rx: mpsc::Receiver<ControlCommand>,
    mut mission_cmd_rx: mpsc::Receiver<crate::tools::mission::MissionControlCommand>,
    mission_cmd_tx: mpsc::Sender<crate::tools::mission::MissionControlCommand>,
    events_tx: broadcast::Sender<AgentEvent>,
    mut events_rx: broadcast::Receiver<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    current_mission: Arc<RwLock<Option<Uuid>>>,
    current_tree: Arc<RwLock<Option<AgentTreeNode>>>,
    progress: Arc<RwLock<ExecutionProgress>>,
    mission_store: Arc<dyn MissionStore>,
    secrets: Option<Arc<SecretsStore>>,
) {
    // Queue stores (id, content, agent, target_mission_id) for the current/primary mission
    // The target_mission_id tracks which mission each queued message is intended for
    let mut queue: VecDeque<(Uuid, String, Option<String>, Option<Uuid>)> = VecDeque::new();
    let mut history: Vec<(String, String)> = Vec::new(); // (role, content) pairs (user/assistant)
    let mut running: Option<tokio::task::JoinHandle<(Uuid, String, crate::agents::AgentResult)>> =
        None;
    let mut running_cancel: Option<CancellationToken> = None;
    // Track which mission the main `running` task is actually working on.
    // This is different from `current_mission` which can change when user creates a new mission.
    let mut running_mission_id: Option<Uuid> = None;
    // Track last activity for the main runner (for stall detection)
    let mut main_runner_last_activity: std::time::Instant = std::time::Instant::now();
    // Track current activity label for the main runner
    let mut main_runner_activity: Option<String> = None;
    // Track subtasks for the main runner
    let mut main_runner_subtasks: Vec<super::mission_runner::SubtaskInfo> = Vec::new();
    // Deadline for force-reaping a runner whose cancel token was fired
    // but whose JoinHandle never resolved. This handles the "zombie
    // runner" case: the underlying CLI subprocess died (or never reacts
    // to the cancel because it's blocked on a closed pipe / dead
    // child), so the spawned task never returns, `running` stays
    // `Some`, the in-memory running list keeps reporting the mission,
    // and `Stop` becomes a no-op. After this deadline we force-abort
    // the JoinHandle and clean up the in-memory state.
    let mut runner_force_clear_deadline: Option<tokio::time::Instant> = None;
    const RUNNER_FORCE_CLEAR_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

    // Parallel mission runners - each runs independently
    let mut parallel_runners: std::collections::HashMap<
        Uuid,
        super::mission_runner::MissionRunner,
    > = std::collections::HashMap::new();

    // Helper to extract file paths from text (for mission summaries)
    fn extract_file_paths(text: &str) -> Vec<String> {
        let mut paths = Vec::new();
        // Match common file path patterns
        for word in text.split_whitespace() {
            let word =
                word.trim_matches(|c| c == '`' || c == '\'' || c == '"' || c == ',' || c == ':');
            if (word.starts_with('/') || word.starts_with("./"))
                && word.len() > 3
                && !word.contains("http")
                && word.chars().filter(|c| *c == '/').count() >= 1
            {
                // Likely a file path
                paths.push(word.to_string());
            }
        }
        paths
    }

    // Helper to persist history to a specific mission ID
    async fn persist_mission_history_to(
        mission_store: &Arc<dyn MissionStore>,
        events_tx: &broadcast::Sender<AgentEvent>,
        mission_id: Option<Uuid>,
        history: &[(String, String)],
    ) {
        if let Some(mid) = mission_id {
            let entries: Vec<MissionHistoryEntry> = history
                .iter()
                .map(|(role, content)| MissionHistoryEntry {
                    role: role.clone(),
                    content: content.clone(),
                })
                .collect();
            persist_mission_history_and_schedule_metadata_refresh(
                mission_store,
                events_tx,
                mid,
                &entries,
            )
            .await;
        }
    }

    // Helper to persist history to current mission (wrapper for backwards compatibility)
    async fn persist_mission_history(
        mission_store: &Arc<dyn MissionStore>,
        events_tx: &broadcast::Sender<AgentEvent>,
        current_mission: &Arc<RwLock<Option<Uuid>>>,
        history: &[(String, String)],
    ) {
        let mission_id = *current_mission.read().await;
        persist_mission_history_to(mission_store, events_tx, mission_id, history).await;
    }

    fn parse_tool_result_object(result: &serde_json::Value) -> Option<serde_json::Value> {
        if result.is_object() {
            return Some(result.clone());
        }
        if let Some(raw) = result.as_str() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
                return Some(parsed);
            }
        }
        None
    }

    // Helper to load a mission and return a Mission struct
    async fn load_mission_record(
        mission_store: &Arc<dyn MissionStore>,
        id: Uuid,
    ) -> Result<Mission, String> {
        mission_store
            .get_mission(id)
            .await?
            .ok_or_else(|| format!("Mission {} not found", id))
    }

    // Helper to create a new mission
    async fn create_new_mission(mission_store: &Arc<dyn MissionStore>) -> Result<Mission, String> {
        create_new_mission_with_title(
            mission_store,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    // Helper to create a new mission with title
    async fn create_new_mission_with_title(
        mission_store: &Arc<dyn MissionStore>,
        title: Option<&str>,
        workspace_id: Option<Uuid>,
        agent: Option<&str>,
        model_override: Option<&str>,
        model_effort: Option<&str>,
        backend: Option<&str>,
        config_profile: Option<&str>,
        parent_mission_id: Option<Uuid>,
        working_directory: Option<&str>,
    ) -> Result<Mission, String> {
        mission_store
            .create_mission_with_parent(
                title,
                workspace_id,
                agent,
                model_override,
                model_effort,
                backend,
                config_profile,
                parent_mission_id,
                working_directory,
            )
            .await
    }

    // Helper to validate and prepare an interrupted or blocked mission for resume.
    async fn resume_mission_impl(
        mission_store: &Arc<dyn MissionStore>,
        config: &Config,
        mission_id: Uuid,
        clean_workspace: bool,
    ) -> Result<(Mission, String), String> {
        let mission = load_mission_record(mission_store, mission_id).await?;

        // Check if mission can be resumed (interrupted, blocked, or failed)
        // Failed missions can be resumed to retry after transient errors (e.g., 529 overloaded)
        if !matches!(
            mission.status,
            MissionStatus::Interrupted | MissionStatus::Blocked | MissionStatus::Failed
        ) {
            return Err(format!(
                "Mission {} cannot be resumed (status: {})",
                mission_id, mission.status
            ));
        }

        // Clean mission context if requested.
        // Missions share the workspace directory, so we avoid deleting project files.
        if clean_workspace {
            let context_root = config.working_dir.join(&config.context.context_dir_name);
            let mission_context_dir = context_root.join(mission_id.to_string());
            tracing::info!(
                mission_id = %mission_id,
                path = %mission_context_dir.display(),
                "Cleaning mission context directory (shared workspace mode)"
            );
            if mission_context_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&mission_context_dir) {
                    tracing::warn!("Failed to clean mission context: {}", e);
                }
            }
            let _ = std::fs::create_dir_all(&mission_context_dir);

            let runtime_file =
                workspace::runtime_workspace_file_path(&config.working_dir, Some(mission_id));
            let _ = std::fs::remove_file(runtime_file);
        }

        Ok((mission, INTERRUPTED_RESUME_PROMPT.to_string()))
    }

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    ControlCommand::UserMessage { id, content, agent: msg_agent, target_mission_id, respond } => {
                        // Smart routing: decide where to send this message based on target_mission_id
                        // and what's currently running.

                        let current_mission_id = *current_mission.read().await;
                        let running_mid = running_mission_id;
                        let main_mission_id = if running_mid.is_some() {
                            running_mid
                        } else {
                            current_mission_id
                        };
                        let main_is_running = running.is_some();

                        // If no explicit target but current_mission differs from the
                        // running mission (i.e., CreateMission switched the pointer),
                        // infer the target as current_mission so it auto-starts in parallel.
                        let effective_target = target_mission_id.or_else(|| {
                            if main_is_running {
                                if let Some(cid) = current_mission_id {
                                    if running_mid != Some(cid) {
                                        tracing::info!(
                                            "Inferred target mission {} (current differs from running {:?})",
                                            cid, running_mid
                                        );
                                        return Some(cid);
                                    }
                                }
                            }
                            None
                        });

                        // Determine if target is already running somewhere
                        let target_in_parallel = effective_target
                            .map(|tid| parallel_runners.contains_key(&tid))
                            .unwrap_or(false);
                        let target_is_main = effective_target
                            .map(|tid| main_mission_id == Some(tid))
                            .unwrap_or(true); // No target = use main

                        // Case 1: Target is already running in parallel_runners - queue to it
                        if let Some(tid) = effective_target {
                            if target_in_parallel {
                                if let Some(runner) = parallel_runners.get_mut(&tid) {
                                    let was_running = runner.is_running();
                                    runner.queue_message(id, content.clone(), msg_agent);
                                    let _ = events_tx.send(AgentEvent::UserMessage {
                                        id,
                                        content: content.clone(),
                                        queued: was_running,
                                        mission_id: Some(tid),
                                    });
                                    // Try to start if not already running
                                    if !runner.is_running() {
                                        runner.start_next(
                                            config.clone(),
                                            Arc::clone(&root_agent),
                                            Arc::clone(&mcp),
                                            Arc::clone(&workspaces),
                                            library.clone(),
                                            events_tx.clone(),
                                            Arc::clone(&tool_hub),
                                            Arc::clone(&status),
                                            mission_cmd_tx.clone(),
                                            Arc::new(RwLock::new(Some(tid))),
                                            secrets.clone(),
                                        );
                                    }
                                    let _ = respond.send(was_running);
                                    continue;
                                }
                            }
                        }

                        // Case 2: Target differs from main → start parallel
                        // When target_mission_id is explicitly set (e.g. from Telegram),
                        // always use parallel execution to avoid hijacking the main session.
                        // When the target is merely inferred (current differs from running),
                        // only start parallel when main is busy.
                        let force_parallel = target_mission_id.is_some(); // explicit target (e.g. Telegram)
                        if let Some(tid) = effective_target {
                            if !target_is_main && (main_is_running || force_parallel) {
                                // Check capacity
                                let parallel_running = parallel_runners.values().filter(|r| r.is_running()).count();
                                let total_running = parallel_running + if main_is_running { 1 } else { 0 };
                                let max_parallel = crate::settings::max_parallel_missions_cached_or(config.max_parallel_missions);

                                if total_running >= max_parallel {
                                    tracing::warn!(
                                        "Cannot start parallel mission {}: max {} reached. \
                                         Dropping targeted message to avoid sending to wrong mission.",
                                        tid, max_parallel
                                    );
                                    let _ = events_tx.send(AgentEvent::Error {
                                        message: format!(
                                            "Cannot start mission {}: max parallel missions ({}) reached",
                                            tid, max_parallel
                                        ),
                                        mission_id: Some(tid),
                                        resumable: true,
                                    });
                                    let _ = respond.send(false);
                                    continue;
                                } else {
                                    // Load mission and start in parallel
                                    match load_mission_record(&mission_store, tid).await {
                                        Ok(mission) => {
                                            // Activate mission: if pending, interrupted, blocked, completed, or failed, update status to active
                                            if matches!(
                                                mission.status,
                                                MissionStatus::Pending
                                                    | MissionStatus::Interrupted
                                                    | MissionStatus::Blocked
                                                    | MissionStatus::Completed
                                                    | MissionStatus::Failed
                                            ) {
                                                tracing::info!(
                                                    "Activating parallel mission {} (was {})",
                                                    tid, mission.status
                                                );
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate parallel mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                            let mut runner = super::mission_runner::MissionRunner::new(
                                                tid,
                                                mission.workspace_id,
                                                mission.agent.clone(),
                                                Some(mission.backend.clone()),
                                                mission.session_id.clone(),
                                                mission.config_profile.clone(),
                                                mission.model_override.clone(),
                                                mission.model_effort.clone(),
                                            );
                                            runner.working_directory = mission.working_directory.clone();
                                            // Load existing history
                                            for entry in &mission.history {
                                                runner.history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            // Queue the message
                                            runner.queue_message(id, content.clone(), msg_agent);
                                            // Emit user message event
                                            let _ = events_tx.send(AgentEvent::UserMessage {
                                                id,
                                                content: content.clone(),
                                                queued: false,
                                                mission_id: Some(tid),
                                            });
                                            // Start execution
                                            runner.start_next(
                                                config.clone(),
                                                Arc::clone(&root_agent),
                                                Arc::clone(&mcp),
                                                Arc::clone(&workspaces),
                                                library.clone(),
                                                events_tx.clone(),
                                                Arc::clone(&tool_hub),
                                                Arc::clone(&status),
                                                mission_cmd_tx.clone(),
                                                Arc::new(RwLock::new(Some(tid))),
                                                secrets.clone(),
                                            );
                                            tracing::info!("Auto-started mission {} in parallel", tid);
                                            parallel_runners.insert(tid, runner);
                                            let _ = respond.send(false);
                                            continue;
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                "Failed to load mission {} for parallel: {}. \
                                                 Dropping targeted message to avoid sending to wrong mission.",
                                                tid, e
                                            );
                                            let _ = events_tx.send(AgentEvent::Error {
                                                message: format!(
                                                    "Failed to load mission {}: {}",
                                                    tid, e
                                                ),
                                                mission_id: Some(tid),
                                                resumable: true,
                                            });
                                            let _ = respond.send(false);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        // Case 3: Queue to main session (default behavior)
                        // Auto-create mission on first message if none exists
                        {
                            let mission_id = *current_mission.read().await;
                            if mission_id.is_none() {
                                // Use effective_target if available, otherwise create new
                                if let Some(tid) = effective_target {
                                    // Load mission history from DB so continuation detection
                                    // works correctly (e.g., after server restart when
                                    // current_mission is None but the mission has prior turns).
                                    if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                        if !mission.history.is_empty() {
                                            history.clear();
                                            for entry in &mission.history {
                                                history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            tracing::info!(
                                                "Loaded {} history entries for target mission {} (first message after session start)",
                                                mission.history.len(), tid
                                            );
                                        }
                                        // Activate mission if it was pending/interrupted/blocked/completed
                                        if matches!(
                                            mission.status,
                                            MissionStatus::Pending
                                                | MissionStatus::Interrupted
                                                | MissionStatus::Blocked
                                                | MissionStatus::Completed
                                                | MissionStatus::Failed
                                        ) {
                                            tracing::info!(
                                                "Activating main mission {} (was {})",
                                                tid, mission.status
                                            );
                                            if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                tracing::warn!("Failed to activate main mission {}: {}", tid, e);
                                            } else {
                                                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                    mission_id: tid,
                                                    status: MissionStatus::Active,
                                                    summary: None,
                                                });
                                            }
                                        }
                                    }
                                    *current_mission.write().await = Some(tid);
                                    tracing::info!("Set current mission to target: {}", tid);
                                } else if let Ok(new_mission) = create_new_mission(&mission_store).await {
                                    *current_mission.write().await = Some(new_mission.id);
                                    tracing::info!("Auto-created mission: {}", new_mission.id);
                                }
                            } else if let Some(tid) = effective_target {
                                if !main_is_running {
                                    if mission_id != Some(tid) {
                                        // Switch main session to target mission
                                        persist_mission_history(
                                            &mission_store,
                                            &events_tx,
                                            &current_mission,
                                            &history,
                                        )
                                        .await;
                                        if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                            history.clear();
                                            for entry in &mission.history {
                                                history.push((entry.role.clone(), entry.content.clone()));
                                            }
                                            // Activate mission if it was pending/interrupted/blocked/completed
                                            if matches!(
                                                mission.status,
                                                MissionStatus::Pending
                                                    | MissionStatus::Interrupted
                                                    | MissionStatus::Blocked
                                                    | MissionStatus::Completed
                                                    | MissionStatus::Failed
                                            ) {
                                                tracing::info!(
                                                    "Activating switched mission {} (was {})",
                                                    tid, mission.status
                                                );
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate switched mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                        }
                                        *current_mission.write().await = Some(tid);
                                        tracing::info!("Switched main session to mission: {}", tid);
                                    } else if !history.iter().any(|(role, _)| role == "assistant") {
                                        // Same mission but no assistant history in memory
                                        // (e.g., after server restart). Reload from database
                                        // so Claude Code continuation detection works correctly.
                                        if let Ok(mission) = load_mission_record(&mission_store, tid).await {
                                            if !mission.history.is_empty() {
                                                history.clear();
                                                for entry in &mission.history {
                                                    history.push((entry.role.clone(), entry.content.clone()));
                                                }
                                                tracing::info!(
                                                    "Reloaded {} history entries for mission {} (session continuity)",
                                                    mission.history.len(), tid
                                                );
                                            }
                                            // Activate mission if it was pending/interrupted/blocked/completed (same mission, reloading)
                                            if matches!(
                                                mission.status,
                                                MissionStatus::Pending
                                                    | MissionStatus::Interrupted
                                                    | MissionStatus::Blocked
                                                    | MissionStatus::Completed
                                                    | MissionStatus::Failed
                                            ) {
                                                tracing::info!(
                                                    "Activating reloaded mission {} (was {})",
                                                    tid, mission.status
                                                );
                                                if let Err(e) = mission_store.update_mission_status(tid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate reloaded mission {}: {}", tid, e);
                                                } else {
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: tid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let was_running = running.is_some();
                        let content_clone = content.clone();
                        // Capture the target mission ID once, before queuing
                        // This ensures we use the same mission_id for events and execution
                        let target_mission_id = *current_mission.read().await;
                        queue.push_back((id, content, msg_agent, target_mission_id));
                        let status_mission_id = if running.is_some() {
                            running_mission_id
                        } else {
                            target_mission_id
                        };
                        set_and_emit_status(
                            &status,
                            &events_tx,
                            if running.is_some() { ControlRunState::Running } else { ControlRunState::Idle },
                            queue.len(),
                            status_mission_id,
                        ).await;
                        if was_running {
                            let _ = events_tx.send(AgentEvent::UserMessage {
                                id,
                                content: content_clone,
                                queued: true,
                                mission_id: target_mission_id,
                            });
                        }
                        if running.is_none() {
                            if let Some((mid, msg, per_msg_agent, msg_target_mid)) = queue.pop_front() {
                                set_and_emit_status(
                                    &status,
                                    &events_tx,
                                    ControlRunState::Running,
                                    queue.len(),
                                    msg_target_mid,
                                ).await;
                                let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: msg_target_mid });

                                // Immediately persist user message so it's visible when loading mission
                                history.push(("user".to_string(), msg.clone()));
                                persist_mission_history_to(
                                    &mission_store,
                                    &events_tx,
                                    msg_target_mid,
                                    &history,
                                )
                                    .await;

                                let cfg = config.clone();
                                let agent = Arc::clone(&root_agent);
                                let mcp_ref = Arc::clone(&mcp);
                                let workspaces_ref = Arc::clone(&workspaces);
                                let library_ref = Arc::clone(&library);
                                let events = events_tx.clone();
                                let tools_hub = Arc::clone(&tool_hub);
                                let status_ref = Arc::clone(&status);
                                let cancel = CancellationToken::new();
                                let hist_snapshot = history.clone();
                                let mission_ctrl = crate::tools::mission::MissionControl {
                                    current_mission_id: Arc::clone(&current_mission),
                                    cmd_tx: mission_cmd_tx.clone(),
                                };
                                let tree_ref = Arc::clone(&current_tree);
                                let progress_ref = Arc::clone(&progress);
                                // Use the mission ID that was captured when message was queued
                                // This prevents race conditions where current_mission changes between queueing and execution
                                let mission_id = msg_target_mid;
                                let (workspace_id, model_override, model_effort, mission_agent, backend_id, session_id, mission_config_profile) = if let Some(mid) = mission_id {
                                    match mission_store.get_mission(mid).await {
                                        Ok(Some(mission)) => {
                                            // Activate mission: if pending, interrupted, blocked, completed, or failed, update status to active
                                            if matches!(
                                                mission.status,
                                                MissionStatus::Pending
                                                    | MissionStatus::Interrupted
                                                    | MissionStatus::Blocked
                                                    | MissionStatus::Completed
                                                    | MissionStatus::Failed
                                            ) {
                                                tracing::info!(
                                                    "Activating mission {} (was {})",
                                                    mid, mission.status
                                                );
                                                if let Err(e) = mission_store.update_mission_status(mid, MissionStatus::Active).await {
                                                    tracing::warn!("Failed to activate mission {}: {}", mid, e);
                                                } else {
                                                    // Notify frontend of status change
                                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                                        mission_id: mid,
                                                        status: MissionStatus::Active,
                                                        summary: None,
                                                    });
                                                }
                                            }
                                            (
                                                Some(mission.workspace_id),
                                                mission.model_override.clone(),
                                                mission.model_effort.clone(),
                                                mission.agent.clone(),
                                                Some(mission.backend.clone()),
                                                mission.session_id.clone(),
                                                mission.config_profile.clone(),
                                            )
                                        }
                                        Ok(None) => {
                                            tracing::warn!(
                                                "Mission {} not found while resolving workspace",
                                                mid
                                            );
                                            (None, None, None, None, None, None, None)
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to load mission {} for workspace: {}",
                                                mid,
                                                e
                                            );
                                            (None, None, None, None, None, None, None)
                                        }
                                    }
                                } else {
                                    (None, None, None, None, None, None, None)
                                };
                                // Per-message agent overrides mission agent
                                let agent_override = per_msg_agent.or(mission_agent);
                                running_cancel = Some(cancel.clone());
                                running_mission_id = mission_id;
                                // Reset activity tracking when new task starts
                                main_runner_last_activity = std::time::Instant::now();
                                main_runner_activity = None;
                                main_runner_subtasks.clear();
                                running = Some(tokio::spawn(async move {
                                    let result = run_single_control_turn(
                                        cfg,
                                        agent,
                                        mcp_ref,
                                        workspaces_ref,
                                        library_ref,
                                        events,
                                        tools_hub,
                                        status_ref,
                                        cancel,
                                        hist_snapshot,
                                        msg.clone(),
                                        Some(mission_ctrl),
                                        tree_ref,
                                        progress_ref,
                                        mission_id,
                                        workspace_id,
                                        backend_id,
                                        model_override,
                                        model_effort,
                                        agent_override,
                                        session_id,
                                        false, // force_session_resume: regular message, not a resume
                                        mission_config_profile,
                                    )
                                    .await;
                                    (mid, msg, result)
                                }));
                            } else {
                                set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0, None).await;
                            }
                        }
                        let _ = respond.send(was_running);
                    }
                    ControlCommand::ToolResult { tool_call_id, name, result } => {
                        // Deliver to the tool hub. resolve() caches the result if
                        // no one has registered yet (resolve-before-register).
                        let _ = tool_hub.resolve(&tool_call_id, result).await;
                        tracing::debug!(tool_call_id = %tool_call_id, name = %name, "ToolResult delivered to hub");
                    }
                    ControlCommand::Cancel => {
                        if let Some(token) = &running_cancel {
                            token.cancel();
                            // Don't send Error event here - the task will complete and send
                            // an AssistantMessage with the cancellation result when it finishes.
                            // Sending both causes duplicate UI messages.
                        } else {
                            let _ = events_tx.send(AgentEvent::Error { message: "No running task to cancel".to_string(), mission_id: None, resumable: false });
                        }
                    }
                    ControlCommand::LoadMission { id, respond } => {
                        // First persist current mission history
                        persist_mission_history(
                            &mission_store,
                            &events_tx,
                            &current_mission,
                            &history,
                        )
                        .await;

                        // Load the new mission
                        match load_mission_record(
                            &mission_store,
                            id,
                        )
                        .await {
                            Ok(mission) => {
                                // Update history from loaded mission
                                history = mission.history.iter()
                                    .map(|e| (e.role.clone(), e.content.clone()))
                                    .collect();
                                *current_mission.write().await = Some(id);

                                // Write runtime workspace state so file uploads work immediately
                                // (without needing to send a message first)
                                let ws = workspace::resolve_workspace(
                                    &workspaces,
                                    &config,
                                    Some(mission.workspace_id),
                                ).await;
                                if let Err(e) = workspace::write_runtime_workspace_state(
                                    &config.working_dir,
                                    &ws,
                                    &ws.path,
                                    Some(id),
                                    &config.context.context_dir_name,
                                ).await {
                                    tracing::warn!("Failed to write runtime workspace state on load: {}", e);
                                }

                                let _ = respond.send(Ok(mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::CreateMission { title, workspace_id, agent, model_override, model_effort, backend, config_profile, parent_mission_id, working_directory, respond } => {
                        // First persist current mission history
                        persist_mission_history(
                            &mission_store,
                            &events_tx,
                            &current_mission,
                            &history,
                        )
                        .await;

                        // Create a new mission with optional title, workspace, agent, and backend
                        match create_new_mission_with_title(
                            &mission_store,
                            title.as_deref(),
                            workspace_id,
                            agent.as_deref(),
                            model_override.as_deref(),
                            model_effort.as_deref(),
                            backend.as_deref(),
                            config_profile.as_deref(),
                            parent_mission_id,
                            working_directory.as_deref(),
                        )
                        .await {
                            Ok(mission) => {
                                history.clear();
                                *current_mission.write().await = Some(mission.id);

                                // Write runtime workspace state so file uploads work immediately
                                let ws = workspace::resolve_workspace(
                                    &workspaces,
                                    &config,
                                    Some(mission.workspace_id),
                                ).await;
                                if let Err(e) = workspace::write_runtime_workspace_state(
                                    &config.working_dir,
                                    &ws,
                                    &ws.path,
                                    Some(mission.id),
                                    &config.context.context_dir_name,
                                ).await {
                                    tracing::warn!("Failed to write runtime workspace state on create: {}", e);
                                }

                                let _ = respond.send(Ok(mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::SetMissionStatus { id, status: new_status, respond } => {
                        let current_id = *current_mission.read().await;
                        if current_id == Some(id) {
                            if let Some(tree) = current_tree.read().await.clone() {
                                if let Err(e) = mission_store.update_mission_tree(id, &tree).await
                                {
                                    tracing::warn!("Failed to save mission tree: {}", e);
                                }
                            }
                        }

                        let result = mission_store
                            .update_mission_status(id, new_status)
                            .await;
                        if result.is_ok() {
                            maybe_schedule_mission_metadata_refresh_for_status(
                                &mission_store,
                                &events_tx,
                                id,
                                new_status,
                            );
                            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                mission_id: id,
                                status: new_status,
                                summary: None,
                            });
                        }
                        let _ = respond.send(result);
                    }
                    ControlCommand::SetMissionTitle { id, title, respond } => {
                        let result = mission_store.update_mission_title(id, &title).await;
                        if result.is_ok() {
                            let _ = events_tx.send(AgentEvent::MissionTitleChanged {
                                mission_id: id,
                                title: title.clone(),
                            });
                            match mission_store.get_mission(id).await {
                                Ok(Some(updated)) => {
                                    record_metadata_refresh_baseline_from_mission(id, &updated);
                                    emit_mission_metadata_updated_event(&events_tx, id, &updated);
                                }
                                Ok(None) => {
                                    tracing::warn!(
                                        "Mission {} disappeared after title update",
                                        id
                                    );
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        "Failed to reload mission {} after title update: {}",
                                        id,
                                        err
                                    );
                                }
                            }
                        }
                        let _ = respond.send(result);
                    }
                    ControlCommand::UpdateMissionSettings { id, backend, agent, model_override, model_effort, config_profile, session_id, respond } => {
                        let main_running = running.is_some() && running_mission_id == Some(id);
                        let parallel_running = parallel_runners
                            .get(&id)
                            .is_some_and(|runner| runner.is_running());
                        if main_running || parallel_running {
                            let _ = respond.send(Err(
                                "Cannot update mission settings while the mission is running"
                                    .to_string(),
                            ));
                            continue;
                        }

                        let result = mission_store
                            .update_mission_run_settings(
                                id,
                                backend.as_deref(),
                                agent.as_ref().map(|value| value.as_deref()),
                                model_override.as_ref().map(|value| value.as_deref()),
                                model_effort.as_ref().map(|value| value.as_deref()),
                                config_profile.as_ref().map(|value| value.as_deref()),
                                &session_id,
                            )
                            .await;

                        if let Ok(updated) = result.as_ref() {
                            let _ = events_tx.send(AgentEvent::MissionSettingsUpdated {
                                mission_id: id,
                                backend: updated.backend.clone(),
                                agent: updated.agent.clone(),
                                model_override: updated.model_override.clone(),
                                model_effort: updated.model_effort.clone(),
                                config_profile: updated.config_profile.clone(),
                                session_id: updated.session_id.clone(),
                                updated_at: updated.updated_at.clone(),
                            });
                        }

                        let _ = respond.send(result);
                    }
                    ControlCommand::StartParallel { mission_id, content, respond } => {
                        tracing::info!("StartParallel requested for mission {}", mission_id);

                        // Count currently running parallel missions
                        let parallel_running = parallel_runners.values().filter(|r| r.is_running()).count();
                        let main_running = if running.is_some() { 1 } else { 0 };
                        let total_running = parallel_running + main_running;
                        let max_parallel = crate::settings::max_parallel_missions_cached_or(config.max_parallel_missions);

                        if total_running >= max_parallel {
                            let _ = respond.send(Err(format!(
                                "Maximum parallel missions ({}) reached. {} running.",
                                max_parallel, total_running
                            )));
                        } else if let std::collections::hash_map::Entry::Vacant(entry) =
                            parallel_runners.entry(mission_id)
                        {
                            // Load mission to get existing history
                            let mission = match load_mission_record(&mission_store, mission_id).await {
                                Ok(m) => m,
                                Err(e) => {
                                    let _ = respond.send(Err(format!("Failed to load mission: {}", e)));
                                    continue;
                                }
                            };

                            // Create a new MissionRunner
                            let mut runner = super::mission_runner::MissionRunner::new(
                                mission_id,
                                mission.workspace_id,
                                mission.agent.clone(),
                                Some(mission.backend.clone()),
                                mission.session_id.clone(),
                                mission.config_profile.clone(),
                                mission.model_override.clone(),
                                mission.model_effort.clone(),
                            );
                            runner.working_directory = mission.working_directory.clone();

                            // Load existing history into runner to preserve conversation context
                            for entry in &mission.history {
                                runner.history.push((entry.role.clone(), entry.content.clone()));
                            }

                            // Queue the initial message (no per-message agent override for parallel start)
                            runner.queue_message(Uuid::new_v4(), content, None);

                            // Start execution
                            let started = runner.start_next(
                                config.clone(),
                                Arc::clone(&root_agent),
                                Arc::clone(&mcp),
                                Arc::clone(&workspaces),
                                library.clone(),
                                events_tx.clone(),
                                Arc::clone(&tool_hub),
                                Arc::clone(&status),
                                mission_cmd_tx.clone(),
                                Arc::new(RwLock::new(Some(mission_id))), // Each runner tracks its own mission
                                secrets.clone(),
                            );

                            if started {
                                if mission.status != MissionStatus::Active {
                                    tracing::info!(
                                        "Activating parallel mission {} (was {})",
                                        mission_id,
                                        mission.status
                                    );
                                    if let Err(e) = mission_store
                                        .update_mission_status(mission_id, MissionStatus::Active)
                                        .await
                                    {
                                        tracing::warn!(
                                            "Failed to activate parallel mission {}: {}",
                                            mission_id,
                                            e
                                        );
                                    } else {
                                        let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                            mission_id,
                                            status: MissionStatus::Active,
                                            summary: None,
                                        });
                                    }
                                }
                                tracing::info!("Mission {} started in parallel", mission_id);
                                entry.insert(runner);
                                let _ = respond.send(Ok(()));
                            } else {
                                let _ = respond.send(Err("Failed to start mission execution".to_string()));
                            }
                        } else {
                            let _ = respond.send(Err(format!(
                                "Mission {} is already running in parallel",
                                mission_id
                            )));
                        }
                    }
                    ControlCommand::CancelMission { mission_id, respond } => {
                        // Helper: cascade-cancel all child missions of the given parent.
                        async fn cancel_child_missions(
                            parent_id: Uuid,
                            mission_store: &Arc<dyn MissionStore>,
                            parallel_runners: &mut HashMap<Uuid, super::mission_runner::MissionRunner>,
                            events_tx: &tokio::sync::broadcast::Sender<AgentEvent>,
                            working_dir: &std::path::Path,
                        ) {
                            let children = match mission_store.get_child_missions(parent_id).await {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!("Failed to fetch child missions for cascade cancel: {}", e);
                                    return;
                                }
                            };
                            for child in children {
                                if matches!(child.status, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Interrupted | MissionStatus::NotFeasible) {
                                    continue;
                                }
                                // Cancel runner if running
                                if let Some(runner) = parallel_runners.get_mut(&child.id) {
                                    runner.cancel();
                                    parallel_runners.remove(&child.id);
                                }
                                if let Err(e) = mission_store
                                    .update_mission_status(child.id, MissionStatus::Interrupted)
                                    .await
                                {
                                    tracing::warn!("Failed to cancel child mission {}: {}", child.id, e);
                                } else {
                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                        mission_id: child.id,
                                        status: MissionStatus::Interrupted,
                                        summary: None,
                                    });
                                }
                                close_mission_desktop_sessions(mission_store, child.id, working_dir).await;
                            }
                        }

                        // First check parallel runners
                        if let Some(runner) = parallel_runners.get_mut(&mission_id) {
                            runner.cancel();
                            // Update status to Interrupted so the mission can be
                            // resumed later (fixes #149: cancel left status as pending).
                            if let Err(e) = mission_store
                                .update_mission_status(mission_id, MissionStatus::Interrupted)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to update cancelled parallel mission status: {}",
                                    e
                                );
                            } else {
                                maybe_schedule_mission_metadata_refresh_for_status(
                                    &mission_store,
                                    &events_tx,
                                    mission_id,
                                    MissionStatus::Interrupted,
                                );
                            }
                            let _ = events_tx.send(AgentEvent::Error {
                                message: format!("Parallel mission {} cancelled", mission_id),
                                mission_id: Some(mission_id),
                                resumable: true, // Cancelled missions can be resumed
                            });
                            let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                mission_id,
                                status: MissionStatus::Interrupted,
                                summary: None,
                            });
                            parallel_runners.remove(&mission_id);
                            close_mission_desktop_sessions(
                                &mission_store,
                                mission_id,
                                &config.working_dir,
                            )
                            .await;
                            // Cascade cancel child missions
                            cancel_child_missions(mission_id, &mission_store, &mut parallel_runners, &events_tx, &config.working_dir).await;
                            let _ = respond.send(Ok(()));
                        } else {
                            // Check if this is the currently executing mission
                            // Use running_mission_id (the actual mission being executed)
                            // instead of current_mission (which can change when user creates a new mission)
                            if running_mission_id == Some(mission_id) {
                                // Cancel the current execution
                                if let Some(token) = &running_cancel {
                                    token.cancel();
                                    // Arm the force-clear deadline. Most cancels
                                    // wind down within a few seconds via the
                                    // task's own observation of the cancel
                                    // token; the deadline catches the zombie
                                    // case where the underlying CLI subprocess
                                    // is dead/blocked and the JoinHandle never
                                    // resolves on its own.
                                    runner_force_clear_deadline = Some(
                                        tokio::time::Instant::now()
                                            + RUNNER_FORCE_CLEAR_GRACE,
                                    );
                                    close_mission_desktop_sessions(
                                        &mission_store,
                                        mission_id,
                                        &config.working_dir,
                                    )
                                    .await;
                                    // Don't send Error event here - the task will complete and send
                                    // an AssistantMessage with resumable=true when it finishes.
                                    // Sending both causes duplicate UI messages.
                                    // Cascade cancel child missions
                                    cancel_child_missions(mission_id, &mission_store, &mut parallel_runners, &events_tx, &config.working_dir).await;
                                    let _ = respond.send(Ok(()));
                                } else {
                                    let _ = respond.send(Err("Mission not currently executing".to_string()));
                                }
                            } else {
                                let _ = respond.send(Err(format!("Mission {} not found", mission_id)));
                            }
                        }
                    }
                    ControlCommand::ListRunning { respond } => {
                        // Return info about currently running missions
                        let mut running_list = Vec::new();

                        // Add main mission if running - use running_mission_id (the actual mission being executed)
                        // instead of current_mission (which can change when user creates a new mission)
                        if running.is_some() {
                            if let Some(mission_id) = running_mission_id {
                                let seconds_since_activity =
                                    main_runner_last_activity.elapsed().as_secs();
                                let state_label = {
                                    let status_guard = status.read().await;
                                    if status_guard.mission_id == Some(mission_id)
                                        && status_guard.state == ControlRunState::WaitingForTool
                                    {
                                        "waiting_for_tool"
                                    } else {
                                        "running"
                                    }
                                };
                                let mission_state = if state_label == "waiting_for_tool" {
                                    super::mission_runner::MissionRunState::WaitingForTool
                                } else {
                                    super::mission_runner::MissionRunState::Running
                                };
                                running_list.push(super::mission_runner::RunningMissionInfo {
                                    mission_id,
                                    state: state_label.to_string(),
                                    queue_len: queue.len(),
                                    history_len: history.len(),
                                    seconds_since_activity,
                                    health: super::mission_runner::running_health(
                                        mission_state,
                                        seconds_since_activity,
                                    ),
                                    expected_deliverables: 0,
                                    current_activity: main_runner_activity.clone(),
                                    subtask_total: main_runner_subtasks.len(),
                                    subtask_completed: main_runner_subtasks.iter().filter(|s| s.completed).count(),
                                });
                            }
                        }

                        // Add all parallel runners
                        for runner in parallel_runners.values() {
                            running_list.push(super::mission_runner::RunningMissionInfo::from(runner));
                        }

                        let _ = respond.send(running_list);
                    }
                    ControlCommand::ResumeMission { mission_id, clean_workspace, skip_message, respond } => {
                        // Resume an interrupted mission by building resume context
                        match resume_mission_impl(
                            &mission_store,
                            &config,
                            mission_id,
                            clean_workspace,
                        )
                        .await {
                            Ok((mission, resume_prompt)) => {
                                let already_running_main =
                                    running.is_some() && running_mission_id == Some(mission_id);
                                let already_running_parallel = parallel_runners
                                    .get(&mission_id)
                                    .is_some_and(|runner| runner.is_running());
                                if already_running_main || already_running_parallel {
                                    let _ = respond.send(Err(format!(
                                        "Mission {} is already running",
                                        mission_id
                                    )));
                                    continue;
                                }

                                // If another main mission is running, resume this one in a
                                // parallel runner so its history stays isolated. This is
                                // important for startup recovery when several missions were
                                // interrupted by the same service restart.
                                if running.is_some() {
                                    if skip_message {
                                        tracing::info!(
                                            mission_id = %mission_id,
                                            "Deferring parallel resume until the caller sends a custom message"
                                        );
                                        let _ = respond.send(Ok(mission));
                                        continue;
                                    }

                                    let parallel_running = parallel_runners
                                        .values()
                                        .filter(|runner| runner.is_running())
                                        .count();
                                    let total_running = parallel_running + 1;
                                    let max_parallel =
                                        crate::settings::max_parallel_missions_cached_or(
                                            config.max_parallel_missions,
                                        );

                                    if total_running >= max_parallel {
                                        let _ = respond.send(Err(format!(
                                            "Maximum parallel missions ({}) reached. {} running.",
                                            max_parallel, total_running
                                        )));
                                        continue;
                                    }

                                    let mut runner = super::mission_runner::MissionRunner::new(
                                        mission_id,
                                        mission.workspace_id,
                                        mission.agent.clone(),
                                        Some(mission.backend.clone()),
                                        mission.session_id.clone(),
                                        mission.config_profile.clone(),
                                        mission.model_override.clone(),
                                        mission.model_effort.clone(),
                                    );
                                    runner.working_directory = mission.working_directory.clone();
                                    for entry in &mission.history {
                                        runner
                                            .history
                                            .push((entry.role.clone(), entry.content.clone()));
                                    }
                                    runner.queue_message(Uuid::new_v4(), resume_prompt, None);

                                    let started = runner.start_next(
                                        config.clone(),
                                        Arc::clone(&root_agent),
                                        Arc::clone(&mcp),
                                        Arc::clone(&workspaces),
                                        library.clone(),
                                        events_tx.clone(),
                                        Arc::clone(&tool_hub),
                                        Arc::clone(&status),
                                        mission_cmd_tx.clone(),
                                        Arc::new(RwLock::new(Some(mission_id))),
                                        secrets.clone(),
                                    );

                                    if !started {
                                        let _ = respond.send(Err(
                                            "Failed to start mission execution".to_string(),
                                        ));
                                        continue;
                                    }

                                    if let Err(e) = mission_store
                                        .update_mission_status(mission_id, MissionStatus::Active)
                                        .await
                                    {
                                        tracing::warn!(
                                            "Failed to resume parallel mission {}: {}",
                                            mission_id,
                                            e
                                        );
                                    } else {
                                        maybe_schedule_mission_metadata_refresh_for_status(
                                            &mission_store,
                                            &events_tx,
                                            mission_id,
                                            MissionStatus::Active,
                                        );
                                        let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                            mission_id,
                                            status: MissionStatus::Active,
                                            summary: None,
                                        });
                                    }

                                    parallel_runners.insert(mission_id, runner);
                                    let mut updated_mission = mission;
                                    updated_mission.status = MissionStatus::Active;
                                    updated_mission.resumable = false;
                                    updated_mission.interrupted_at = None;
                                    let _ = respond.send(Ok(updated_mission));
                                    continue;
                                }

                                // First persist current mission history (if any)
                                persist_mission_history(
                                    &mission_store,
                                    &events_tx,
                                    &current_mission,
                                    &history,
                                )
                                .await;

                                // Load the mission's history into current state
                                history = mission.history.iter()
                                    .map(|e| (e.role.clone(), e.content.clone()))
                                    .collect();
                                *current_mission.write().await = Some(mission_id);

                                // Update mission status back to active
                                if let Err(e) = mission_store
                                    .update_mission_status(mission_id, MissionStatus::Active)
                                    .await
                                {
                                    tracing::warn!("Failed to resume mission {}: {}", mission_id, e);
                                } else {
                                    maybe_schedule_mission_metadata_refresh_for_status(
                                        &mission_store,
                                        &events_tx,
                                        mission_id,
                                        MissionStatus::Active,
                                    );
                                    // Send status changed event so UI updates
                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                        mission_id,
                                        status: MissionStatus::Active,
                                        summary: None,
                                    });
                                }

                                // Queue the resume prompt as a message (no per-message agent override)
                                // Skip if the caller just wants to update the status (e.g., before sending a custom message)
                                if !skip_message {
                                    let msg_id = Uuid::new_v4();
                                    queue.push_back((msg_id, resume_prompt, None, Some(mission_id)));
                                }

                                // Start execution if not already running
                                if running.is_none() {
                                    if let Some((mid, msg, _per_msg_agent, msg_target_mid)) = queue.pop_front() {
                                        let target_mid = msg_target_mid.unwrap_or(mission_id);
                                        set_and_emit_status(
                                            &status,
                                            &events_tx,
                                            ControlRunState::Running,
                                            queue.len(),
                                            Some(target_mid),
                                        ).await;
                                        let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: Some(target_mid) });
                                        let cfg = config.clone();
                                        let agent = Arc::clone(&root_agent);
                                        let mcp_ref = Arc::clone(&mcp);
                                        let workspaces_ref = Arc::clone(&workspaces);
                                        let library_ref = Arc::clone(&library);
                                        let events = events_tx.clone();
                                        let tools_hub = Arc::clone(&tool_hub);
                                        let status_ref = Arc::clone(&status);
                                        let cancel = CancellationToken::new();
                                        let hist_snapshot = history.clone();
                                        let mission_ctrl = crate::tools::mission::MissionControl {
                                            current_mission_id: Arc::clone(&current_mission),
                                            cmd_tx: mission_cmd_tx.clone(),
                                        };
                                        let tree_ref = Arc::clone(&current_tree);
                                        let progress_ref = Arc::clone(&progress);
                                        let workspace_id = Some(mission.workspace_id);
                                        let backend_id = Some(mission.backend.clone());
                                        let model_override = mission.model_override.clone();
                                        let model_effort = mission.model_effort.clone();
                                        // Resume uses mission agent (no per-message override for resumes)
                                        let agent_override = mission.agent.clone();
                                        let session_id = mission.session_id.clone();
                                        let mission_config_profile = mission.config_profile.clone();
                                        running_cancel = Some(cancel.clone());
                                        // Capture which mission this task is working on (the resumed mission)
                                        running_mission_id = Some(mission_id);
                                        // Reset activity tracking so stall detection starts fresh
                                        main_runner_last_activity = std::time::Instant::now();
                                        main_runner_activity = None;
                                        main_runner_subtasks.clear();
                                        running = Some(tokio::spawn(async move {
                                            let result = run_single_control_turn(
                                                cfg,
                                                agent,
                                                mcp_ref,
                                                workspaces_ref,
                                                library_ref,
                                                events,
                                                tools_hub,
                                                status_ref,
                                                cancel,
                                                hist_snapshot,
                                                msg.clone(),
                                                Some(mission_ctrl),
                                                tree_ref,
                                                progress_ref,
                                                Some(mission_id),
                                                workspace_id,
                                                backend_id,
                                                model_override,
                                                model_effort,
                                                agent_override,
                                                session_id,
                                                true, // force_session_resume: this is a resume operation
                                                mission_config_profile,
                                            )
                                            .await;
                                            (mid, msg, result)
                                        }));
                                    }
                                }

                                // Return the updated mission
                                let mut updated_mission = mission;
                                updated_mission.status = MissionStatus::Active;
                                updated_mission.resumable = false;
                                updated_mission.interrupted_at = None;
                                let _ = respond.send(Ok(updated_mission));
                            }
                            Err(e) => {
                                let _ = respond.send(Err(e));
                            }
                        }
                    }
                    ControlCommand::GracefulShutdown { respond } => {
                        // Mark all running missions as interrupted
                        let mut interrupted_ids = Vec::new();

                        // Handle main mission - use running_mission_id (the actual mission being executed)
                        // Note: We DON'T persist history here because:
                        // 1. If current_mission == running_mission_id, history is correct
                        // 2. If current_mission != running_mission_id (user created new mission),
                        //    history was cleared and doesn't belong to running_mission_id
                        // The running mission's history is already in DB from previous exchanges,
                        // and any in-progress exchange will be lost (acceptable for shutdown).
                        if running.is_some() {
                            if let Some(mission_id) = running_mission_id {
                                // Only persist if the running mission is still current mission
                                // (i.e., user didn't create a new mission while this one was running)
                                let current_mid = *current_mission.read().await;
                                if current_mid == Some(mission_id) {
                                    persist_mission_history(
                                        &mission_store,
                                        &events_tx,
                                        &current_mission,
                                        &history,
                                    )
                                    .await;
                                }
                                // Note: If missions differ, don't persist - the local history
                                // belongs to current_mission, not running_mission_id

                                if mission_store
                                    .update_mission_status_with_reason(
                                        mission_id,
                                        MissionStatus::Interrupted,
                                        Some("server_shutdown"),
                                    )
                                    .await
                                    .is_ok()
                                {
                                    maybe_schedule_mission_metadata_refresh_for_status(
                                        &mission_store,
                                        &events_tx,
                                        mission_id,
                                        MissionStatus::Interrupted,
                                    );
                                    interrupted_ids.push(mission_id);
                                    tracing::info!("Marked mission {} as interrupted", mission_id);
                                }

                                // Cancel execution
                                if let Some(token) = &running_cancel {
                                    token.cancel();
                                }
                            }
                        }

                        // Handle parallel missions
                        for (mission_id, runner) in parallel_runners.iter_mut() {
                            // Persist history for parallel mission
                            let entries: Vec<MissionHistoryEntry> = runner
                                .history
                                .iter()
                                .map(|(role, content)| MissionHistoryEntry {
                                    role: role.clone(),
                                    content: content.clone(),
                                })
                                .collect();
                            persist_mission_history_and_schedule_metadata_refresh(
                                &mission_store,
                                &events_tx,
                                *mission_id,
                                &entries,
                            )
                            .await;
                            if mission_store
                                .update_mission_status_with_reason(
                                    *mission_id,
                                    MissionStatus::Interrupted,
                                    Some("server_shutdown"),
                                )
                                .await
                                .is_ok()
                            {
                                maybe_schedule_mission_metadata_refresh_for_status(
                                    &mission_store,
                                    &events_tx,
                                    *mission_id,
                                    MissionStatus::Interrupted,
                                );
                                interrupted_ids.push(*mission_id);
                                tracing::info!("Marked parallel mission {} as interrupted", mission_id);
                            }

                            runner.cancel();
                        }

                        let _ = respond.send(interrupted_ids);
                    }
                    ControlCommand::GetQueue { respond } => {
                        // Collect queued messages from main runner with their target mission IDs
                        let mut queued: Vec<QueuedMessage> = queue
                            .iter()
                            .map(|(id, content, agent, target_mid)| QueuedMessage {
                                id: *id,
                                content: content.clone(),
                                agent: agent.clone(),
                                mission_id: *target_mid,
                            })
                            .collect();
                        // Also collect queued messages from parallel runners
                        for (mid, runner) in parallel_runners.iter() {
                            for qm in runner.queue.iter() {
                                queued.push(QueuedMessage {
                                    id: qm.id,
                                    content: qm.content.clone(),
                                    agent: qm.agent.clone(),
                                    mission_id: Some(*mid),
                                });
                            }
                        }
                        let _ = respond.send(queued);
                    }
                    ControlCommand::RemoveFromQueue { message_id, respond } => {
                        let mut removed = false;

                        // Try to remove from main queue
                        let before_len = queue.len();
                        queue.retain(|(id, _, _, _)| *id != message_id);
                        if queue.len() < before_len {
                            removed = true;
                            // Emit event for main queue change
                            let _ = events_tx.send(AgentEvent::Status {
                                state: if running.is_some() {
                                    ControlRunState::Running
                                } else {
                                    ControlRunState::Idle
                                },
                                queue_len: queue.len(),
                                mission_id: if running.is_some() {
                                    running_mission_id
                                } else {
                                    *current_mission.read().await
                                },
                            });
                        }

                        // Also try to remove from parallel runner queues
                        for (mid, runner) in parallel_runners.iter_mut() {
                            if runner.remove_from_queue(message_id) {
                                removed = true;
                                tracing::info!("Removed message {} from parallel mission {}", message_id, mid);
                            }
                        }

                        let _ = respond.send(removed);
                    }
                    ControlCommand::ClearQueue { respond } => {
                        let mut cleared = queue.len();
                        queue.clear();

                        // Also clear parallel runner queues
                        for (_mid, runner) in parallel_runners.iter_mut() {
                            cleared += runner.clear_queue();
                        }

                        // Emit event to notify frontend (main queue only)
                        let _ = events_tx.send(AgentEvent::Status {
                            state: if running.is_some() {
                                ControlRunState::Running
                            } else {
                                ControlRunState::Idle
                            },
                            queue_len: 0,
                            mission_id: if running.is_some() {
                                running_mission_id
                            } else {
                                *current_mission.read().await
                            },
                        });

                        tracing::info!("Cleared {} total queued messages (main + parallel)", cleared);
                        let _ = respond.send(cleared);
                    }
                }
            }
            // Handle agent-initiated mission status changes (from complete_mission tool)
            mission_cmd = mission_cmd_rx.recv() => {
                if let Some(cmd) = mission_cmd {
                    match cmd {
                        crate::tools::mission::MissionControlCommand::SetStatus { mission_id: id, status, summary } => {
                            let new_status = match status {
                                crate::tools::mission::MissionStatusValue::Completed => MissionStatus::Completed,
                                crate::tools::mission::MissionStatusValue::Failed => MissionStatus::Failed,
                                crate::tools::mission::MissionStatusValue::Blocked => MissionStatus::Blocked,
                                crate::tools::mission::MissionStatusValue::NotFeasible => MissionStatus::NotFeasible,
                            };
                            let success = matches!(status, crate::tools::mission::MissionStatusValue::Completed);
                            if new_status == MissionStatus::Completed
                                && mission_has_active_automation(&mission_store, id).await
                            {
                                tracing::info!(
                                    "Skipping completion for mission {} because active automations are enabled",
                                    id
                                );
                                continue;
                            }
                            // Save the final tree before updating status
                            if let Some(tree) = current_tree.read().await.clone() {
                                if let Err(e) = mission_store.update_mission_tree(id, &tree).await {
                                    tracing::warn!("Failed to save mission tree: {}", e);
                                } else {
                                    tracing::info!("Saved final tree for mission {}", id);
                                }
                            }

                            if mission_store
                                .update_mission_status(id, new_status)
                                .await
                                .is_ok()
                            {
                                maybe_schedule_mission_metadata_refresh_for_status(
                                    &mission_store,
                                    &events_tx,
                                    id,
                                    new_status,
                                );
                                // Generate and store mission summary
                                if let Some(ref summary_text) = summary {
                                    // Extract key files from conversation (look for paths in assistant messages)
                                    let key_files: Vec<String> = history
                                        .iter()
                                        .filter(|(role, _)| role == "assistant")
                                        .flat_map(|(_, content)| extract_file_paths(content))
                                        .take(10)
                                        .collect();

                                    if let Err(e) = mission_store
                                        .insert_mission_summary(id, summary_text, &key_files, success)
                                        .await
                                    {
                                        tracing::warn!("Failed to store mission summary: {}", e);
                                    } else {
                                        tracing::info!("Stored mission summary for {}", id);
                                    }
                                }

                                let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                    mission_id: id,
                                    status: new_status,
                                    summary,
                                });
                                tracing::info!("Mission {} marked as {} by agent", id, new_status);
                            }
                        }
                    }
                }
            }
            finished = async {
                match &mut running {
                    Some(handle) => Some(handle.await),
                    None => None
                }
            }, if running.is_some() => {
                if let Some(res) = finished {
                    // Save the running mission ID before clearing it - we need it for persist and auto-complete
                    // (current_mission can change if user clicks "New Mission" while task was running)
                    let completed_mission_id = running_mission_id;
                    running = None;
                    running_cancel = None;
                    running_mission_id = None;
                    main_runner_activity = None;
                    // Runner cleared itself; cancel the force-clear watchdog.
                    runner_force_clear_deadline = None;
                    let mut completed_terminal_reason = None;
                    match res {
                        Ok((_mid, _user_msg, mut agent_result)) => {
                            maybe_recover_soft_llm_error(&mut agent_result);
                            completed_terminal_reason = agent_result.terminal_reason;
                            // Only append assistant to local history if this mission is still the current mission.
                            // Note: User message was already added before execution started.
                            // If the user created a new mission mid-execution, history was cleared for that new mission,
                            // and we don't want to contaminate it with the old mission's exchange.
                            let current_mid = *current_mission.read().await;
                            if completed_mission_id == current_mid {
                                history.push(("assistant".to_string(), agent_result.output.clone()));
                            }

                            // Persist to mission using the actual completed mission ID
                            // (not current_mission, which could have changed)
                            //
                            // IMPORTANT: We fetch existing history from DB and append, rather than
                            // using the local `history` variable, because CreateMission may have
                            // cleared `history` while this task was running. This prevents data loss.
                            // Note: User message was already persisted before execution started.
                            if let Some(mid) = completed_mission_id {
                                match mission_store.get_mission(mid).await {
                                    Ok(Some(mission)) => {
                                        let mut entries = mission.history.clone();
                                        entries.push(MissionHistoryEntry {
                                            role: "assistant".to_string(),
                                            content: agent_result.output.clone(),
                                        });
                                        if let Err(e) =
                                            mission_store.update_mission_history(mid, &entries).await
                                        {
                                            tracing::warn!("Failed to persist mission history: {}", e);
                                        } else {
                                            schedule_mission_metadata_refresh(
                                                &mission_store,
                                                &events_tx,
                                                mid,
                                                false,
                                            );
                                        }
                                    }
                                    Ok(None) => {
                                        tracing::warn!("Mission {} not found for history append", mid);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Failed to load mission {} for history append: {}",
                                            mid,
                                            e
                                        );
                                    }
                                }
                            }

                            // P1 FIX: Auto-complete mission if agent execution ended in a terminal state
                            // without an explicit complete_mission call.
                            // This prevents missions from staying "active" forever after max iterations, stalls, etc.
                            //
                            // We use terminal_reason (structured enum) instead of substring matching to avoid
                            // false positives when agent output legitimately contains words like "infinite loop".
                            // We also check the current mission status from DB to handle:
                            // - Explicit complete_mission calls (which update DB status)
                            // - Parallel missions (each has its own DB status)
                            if let Some(mission_id) = completed_mission_id {
                                maybe_finalize_terminal_mission(
                                    &mission_store,
                                    &events_tx,
                                    mission_id,
                                    agent_result.terminal_reason,
                                    false,
                                    "turn finished before follow-up enqueue",
                                )
                                .await;
                            }

                            // Parse rich tags and validate referenced files
                            let rich_tags = parse_rich_tags(&agent_result.output);
                            let shared_files = if rich_tags.is_empty() {
                                None
                            } else {
                                // Get workspace_id from mission for download URLs
                                let ws_id = if let Some(mid) = completed_mission_id {
                                    mission_store
                                        .get_mission(mid)
                                        .await
                                        .ok()
                                        .flatten()
                                        .map(|m| m.workspace_id)
                                } else {
                                    None
                                };
                                // Validate against the per-mission workspace directory, not the global
                                // server working_dir. Agent-relative paths (./foo.png) should resolve
                                // to the mission workspace.
                                let validate_root = if let (Some(mid), Some(wsid)) =
                                    (completed_mission_id, ws_id)
                                {
                                    workspaces
                                        .get(wsid)
                                        .await
                                        .map(|w| crate::workspace::mission_workspace_dir_for_root(&w.path, mid))
                                        .unwrap_or_else(|| config.working_dir.clone())
                                } else {
                                    config.working_dir.clone()
                                };
                                let files = validate_rich_tags(
                                    &rich_tags,
                                    &validate_root,
                                    ws_id,
                                    completed_mission_id,
                                )
                                .await;
                                if files.is_empty() { None } else { Some(files) }
                            };

                            // Mark failures as resumable so UI can show a resume button
                            let resumable = !agent_result.success && completed_mission_id.is_some();
                            let model_used = agent_result.model_used.clone();
                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                id: Uuid::new_v4(),
                                content: agent_result.output.clone(),
                                success: agent_result.success,
                                cost_cents: agent_result.cost_cents,
                                cost_source: agent_result.cost_source,
                                usage: agent_result.usage.clone(),
                                model: model_used.clone(),
                                model_normalized: model_used
                                    .as_deref()
                                    .map(crate::cost::normalized_model),
                                mission_id: completed_mission_id,
                                shared_files,
                                resumable,
                            });
                            if let Some(mission_id) = completed_mission_id {
                                // Update automation executions based on agent outcome
                                let error_msg = if agent_result.success {
                                    None
                                } else {
                                    Some(
                                        agent_result.terminal_reason
                                            .map(|r| format!("{:?}", r))
                                            .unwrap_or_else(|| "Agent execution failed".to_string()),
                                    )
                                };
                                if let Err(e) = mission_store
                                    .complete_running_executions_for_mission(
                                        mission_id,
                                        agent_result.success,
                                        error_msg,
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to complete running executions for mission {}: {}",
                                        mission_id,
                                        e
                                    );
                                }

                                close_mission_desktop_sessions(
                                    &mission_store,
                                    mission_id,
                                    &config.working_dir,
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let _ = events_tx.send(AgentEvent::Error {
                                message: format!("Control session task join failed: {}", e),
                                mission_id: completed_mission_id,
                                resumable: completed_mission_id.is_some(), // Can resume if mission exists
                            });
                            if let Some(mission_id) = completed_mission_id {
                                // Mark running automation executions as failed
                                if let Err(e2) = mission_store
                                    .complete_running_executions_for_mission(
                                        mission_id,
                                        false,
                                        Some(format!("Task join failed: {}", e)),
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to complete running executions for mission {}: {}",
                                        mission_id,
                                        e2
                                    );
                                }

                                // Update mission status so it doesn't stay Active forever.
                                // Mark as Failed (resumable) so the user can retry.
                                if let Err(e) = mission_store
                                    .update_mission_status(mission_id, MissionStatus::Failed)
                                    .await
                                {
                                    tracing::warn!("Failed to update mission status after join error: {}", e);
                                } else {
                                    maybe_schedule_mission_metadata_refresh_for_status(
                                        &mission_store,
                                        &events_tx,
                                        mission_id,
                                        MissionStatus::Failed,
                                    );
                                    let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                                        mission_id,
                                        status: MissionStatus::Failed,
                                        summary: Some("Task execution failed unexpectedly".to_string()),
                                    });
                                }
                                close_mission_desktop_sessions(
                                    &mission_store,
                                    mission_id,
                                    &config.working_dir,
                                )
                                .await;
                            }
                        }
                    }

                    // If the mission is idle now, enqueue any agent_finished automations after a short delay.
                    // Skip automations for transient infrastructure failures (auth errors,
                    // rate limits, capacity limits) — these aren't "the agent finished work",
                    // they're transient failures that the retry/recovery logic handles.
                    // Firing automations on these creates noisy retry loops.
                    if let Some(mission_id) = completed_mission_id {
                        let is_transient_infra_failure = matches!(
                            completed_terminal_reason,
                            Some(TerminalReason::AuthError)
                                | Some(TerminalReason::RateLimited)
                                | Some(TerminalReason::CapacityLimited)
                        );
                        let already_queued_for_mission = queue
                            .iter()
                            .any(|(_id, _msg, _agent, target_mid)| *target_mid == Some(mission_id));
                        if !already_queued_for_mission && !is_transient_infra_failure {
                            // Small delay so the UI can display the completion before restarting.
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            let messages = agent_finished_automation_messages(
                                &mission_store,
                                mission_id,
                                &library,
                                &workspaces,
                            )
                            .await;
                            enqueue_agent_finished_messages(&mut queue, messages);
                        }

                        if !queue_has_pending_target_mission(&queue, mission_id) {
                            maybe_finalize_terminal_mission(
                                &mission_store,
                                &events_tx,
                                mission_id,
                                completed_terminal_reason,
                                true,
                                "turn finished with no same-mission follow-up queued",
                            )
                            .await;
                        }
                    }
                }

                // Start next queued message, if any.
                if let Some((mid, msg, per_msg_agent, msg_target_mid)) = queue.pop_front() {
                    set_and_emit_status(
                        &status,
                        &events_tx,
                        ControlRunState::Running,
                        queue.len(),
                        msg_target_mid,
                    ).await;
                    let _ = events_tx.send(AgentEvent::UserMessage { id: mid, content: msg.clone(), queued: false, mission_id: msg_target_mid });

                    // Immediately persist user message so it's visible when loading mission
                    history.push(("user".to_string(), msg.clone()));
                    persist_mission_history_to(
                        &mission_store,
                        &events_tx,
                        msg_target_mid,
                        &history,
                    )
                        .await;

                    let cfg = config.clone();
                    let agent = Arc::clone(&root_agent);
                    let mcp_ref = Arc::clone(&mcp);
                    let workspaces_ref = Arc::clone(&workspaces);
                    let library_ref = Arc::clone(&library);
                    let events = events_tx.clone();
                    let tools_hub = Arc::clone(&tool_hub);
                    let status_ref = Arc::clone(&status);
                    let cancel = CancellationToken::new();
                    let hist_snapshot = history.clone();
                    let mission_ctrl = crate::tools::mission::MissionControl {
                        current_mission_id: Arc::clone(&current_mission),
                        cmd_tx: mission_cmd_tx.clone(),
                    };
                    let tree_ref = Arc::clone(&current_tree);
                    let progress_ref = Arc::clone(&progress);
                    running_cancel = Some(cancel.clone());
                    // Use the mission ID that was captured when message was queued
                    // This prevents race conditions where current_mission changes between queueing and execution
                    let mission_id = msg_target_mid;
                    let (workspace_id, model_override, model_effort, mission_agent, backend_id, session_id, mission_config_profile) = if let Some(mid) = mission_id {
                        match mission_store.get_mission(mid).await {
                            Ok(Some(mission)) => (
                                Some(mission.workspace_id),
                                mission.model_override.clone(),
                                mission.model_effort.clone(),
                                mission.agent.clone(),
                                Some(mission.backend.clone()),
                                mission.session_id.clone(),
                                mission.config_profile.clone(),
                            ),
                            Ok(None) => {
                                tracing::warn!(
                                    "Mission {} not found while resolving workspace",
                                    mid
                                );
                                (None, None, None, None, None, None, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to load mission {} for workspace: {}",
                                    mid,
                                    e
                                );
                                (None, None, None, None, None, None, None)
                            }
                        }
                    } else {
                        (None, None, None, None, None, None, None)
                    };
                    // Per-message agent overrides mission agent
                    let agent_override = per_msg_agent.or(mission_agent);
                    running_mission_id = mission_id;
                    // Reset activity tracking when new task starts
                    main_runner_last_activity = std::time::Instant::now();
                    main_runner_activity = None;
                    main_runner_subtasks.clear();
                    running = Some(tokio::spawn(async move {
                        let result = run_single_control_turn(
                            cfg,
                            agent,
                            mcp_ref,
                            workspaces_ref,
                            library_ref,
                            events,
                            tools_hub,
                            status_ref,
                            cancel,
                            hist_snapshot,
                            msg.clone(),
                            Some(mission_ctrl),
                            tree_ref,
                            progress_ref,
                            mission_id,
                            workspace_id,
                            backend_id,
                            model_override,
                            model_effort,
                            agent_override,
                            session_id,
                            false, // force_session_resume: continuation turn, not a resume
                            mission_config_profile,
                        )
                        .await;
                        (mid, msg, result)
                    }));
                } else {
                    set_and_emit_status(&status, &events_tx, ControlRunState::Idle, 0, None).await;
                }
            }
            // Poll parallel runners for completion
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                let mut completed_missions = Vec::new();

                for (mission_id, runner) in parallel_runners.iter_mut() {
                    if runner.check_finished() {
                        if let Some((_msg_id, _user_msg, mut result)) = runner.poll_completion().await {
                            maybe_recover_soft_llm_error(&mut result);
                            tracing::info!(
                                "Parallel mission {} completed (success: {}, cost: {} cents)",
                                mission_id, result.success, result.cost_cents
                            );

                            // Parse rich tags and validate referenced files
                            let rich_tags = parse_rich_tags(&result.output);
                            let shared_files = if rich_tags.is_empty() {
                                None
                            } else {
                                let ws_id = mission_store
                                    .get_mission(*mission_id)
                                    .await
                                    .ok()
                                    .flatten()
                                    .map(|m| m.workspace_id);
                                let validate_root = if let Some(wsid) = ws_id {
                                    workspaces
                                        .get(wsid)
                                        .await
                                        .map(|w| crate::workspace::mission_workspace_dir_for_root(&w.path, *mission_id))
                                        .unwrap_or_else(|| config.working_dir.clone())
                                } else {
                                    config.working_dir.clone()
                                };
                                let files = validate_rich_tags(
                                    &rich_tags,
                                    &validate_root,
                                    ws_id,
                                    Some(*mission_id),
                                )
                                .await;
                                if files.is_empty() { None } else { Some(files) }
                            };

                            // Emit completion event with mission_id
                            // Mark failures as resumable
                            let resumable = !result.success;
                            let _ = events_tx.send(AgentEvent::AssistantMessage {
                                // Use a unique id so we don't overwrite the user_message event
                                // (event_id is used for de-dupe in the SQLite event logger).
                                id: Uuid::new_v4(),
                                content: result.output.clone(),
                                success: result.success,
                                cost_cents: result.cost_cents,
                                cost_source: result.cost_source,
                                usage: result.usage.clone(),
                                model: result.model_used.clone(),
                                model_normalized: result
                                    .model_used
                                    .as_deref()
                                    .map(crate::cost::normalized_model),
                                mission_id: Some(*mission_id),
                                shared_files,
                                resumable,
                            });

                            // Update automation executions based on agent outcome
                            {
                                let error_msg = if result.success {
                                    None
                                } else {
                                    Some(
                                        result.terminal_reason
                                            .map(|r| format!("{:?}", r))
                                            .unwrap_or_else(|| "Agent execution failed".to_string()),
                                    )
                                };
                                if let Err(e) = mission_store
                                    .complete_running_executions_for_mission(
                                        *mission_id,
                                        result.success,
                                        error_msg,
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to complete running executions for parallel mission {}: {}",
                                        mission_id,
                                        e
                                    );
                                }
                            }

                            // Persist history for this mission
                            let entries: Vec<MissionHistoryEntry> = runner
                                .history
                                .iter()
                                .map(|(role, content)| MissionHistoryEntry {
                                    role: role.clone(),
                                    content: content.clone(),
                                })
                                .collect();
                            persist_mission_history_and_schedule_metadata_refresh(
                                &mission_store,
                                &events_tx,
                                *mission_id,
                                &entries,
                            )
                            .await;

                            // Check if we should enqueue agent_finished automations.
                            // Skip for transient infrastructure failures (auth, rate limit,
                            // capacity) to avoid noisy retry loops.
                            let is_transient_infra_failure = matches!(
                                result.terminal_reason,
                                Some(TerminalReason::AuthError)
                                    | Some(TerminalReason::RateLimited)
                                    | Some(TerminalReason::CapacityLimited)
                            );
                            let was_queue_empty = runner.queue.is_empty();
                            if was_queue_empty && !is_transient_infra_failure {
                                // Small delay so the UI can display the completion before restarting.
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                                let messages = agent_finished_automation_messages(
                                    &mission_store,
                                    *mission_id,
                                    &library,
                                    &workspaces,
                                )
                                .await;
                                for message in messages {
                                    if message.target_mission_id == *mission_id {
                                        runner.queue_message(Uuid::new_v4(), message.content, None);
                                        continue;
                                    }

                                    // Queue for targeted mission on the main runner if it is not
                                    // the current parallel mission.
                                    queue.push_back((
                                        Uuid::new_v4(),
                                        message.content,
                                        None,
                                        Some(message.target_mission_id),
                                    ));
                                }
                            }

                            // Always try to start next queued message (if any)
                            if !runner.is_running() {
                                // Refresh session_id from the store in case a
                                // SessionIdUpdate event hasn't been processed yet
                                // (race between the events_rx and sleep poll arms).
                                if let Ok(Some(m)) = mission_store.get_mission(*mission_id).await {
                                    if m.session_id != runner.session_id {
                                        tracing::debug!(
                                            mission_id = %mission_id,
                                            old = ?runner.session_id,
                                            new = ?m.session_id,
                                            "Refreshed runner session_id from store"
                                        );
                                        runner.session_id = m.session_id;
                                    }
                                }
                                let started = runner.start_next(
                                    config.clone(),
                                    Arc::clone(&root_agent),
                                    Arc::clone(&mcp),
                                    Arc::clone(&workspaces),
                                    library.clone(),
                                    events_tx.clone(),
                                    Arc::clone(&tool_hub),
                                    Arc::clone(&status),
                                    mission_cmd_tx.clone(),
                                    Arc::new(RwLock::new(Some(*mission_id))),
                                    secrets.clone(),
                                );

                                // If no queued messages, update status and mark for cleanup
                                if !started {
                                    maybe_finalize_terminal_mission(
                                        &mission_store,
                                        &events_tx,
                                        *mission_id,
                                        result.terminal_reason,
                                        true,
                                        "parallel turn finished with no follow-up queued",
                                    )
                                    .await;
                                    completed_missions.push(*mission_id);
                                }
                            }
                        }
                    }
                }

                // Remove completed runners and clean up their desktop sessions
                for mid in completed_missions {
                    parallel_runners.remove(&mid);
                    close_mission_desktop_sessions(
                        &mission_store,
                        mid,
                        &config.working_dir,
                    )
                    .await;
                    tracing::info!("Parallel mission {} removed from runners", mid);
                }
            }
            // Force-reap a runner whose cancel was fired but whose
            // JoinHandle never resolved within the grace window. See
            // `runner_force_clear_deadline` for context.
            _ = async {
                match runner_force_clear_deadline {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => std::future::pending::<()>().await,
                }
            }, if runner_force_clear_deadline.is_some() && running.is_some() => {
                let stuck_mid = running_mission_id;
                tracing::warn!(
                    mission_id = ?stuck_mid,
                    "Force-aborting stuck runner: cancel fired but JoinHandle never resolved within {}s",
                    RUNNER_FORCE_CLEAR_GRACE.as_secs()
                );
                if let Some(handle) = running.take() {
                    handle.abort();
                }
                running_cancel = None;
                running_mission_id = None;
                main_runner_activity = None;
                runner_force_clear_deadline = None;
                if let Some(mid) = stuck_mid {
                    // Mark mission as Interrupted so it stays resumable.
                    if let Err(e) = mission_store
                        .update_mission_status(mid, MissionStatus::Interrupted)
                        .await
                    {
                        tracing::warn!(
                            mission_id = %mid,
                            "Failed to mark force-cleared mission as Interrupted: {}",
                            e
                        );
                    } else {
                        maybe_schedule_mission_metadata_refresh_for_status(
                            &mission_store,
                            &events_tx,
                            mid,
                            MissionStatus::Interrupted,
                        );
                        let _ = events_tx.send(AgentEvent::MissionStatusChanged {
                            mission_id: mid,
                            status: MissionStatus::Interrupted,
                            summary: Some(
                                "Cancel timed out; force-aborted stuck runner.".to_string(),
                            ),
                        });
                    }
                    if let Err(e) = mission_store
                        .complete_running_executions_for_mission(
                            mid,
                            false,
                            Some("Force-aborted stuck runner after cancel timed out".to_string()),
                        )
                        .await
                    {
                        tracing::warn!(
                            mission_id = %mid,
                            "Failed to complete running executions on force-clear: {}",
                            e
                        );
                    }
                    close_mission_desktop_sessions(
                        &mission_store,
                        mid,
                        &config.working_dir,
                    )
                    .await;
                }
            }
            // Update last_activity for runners when we receive events for them
            event = events_rx.recv() => {
                if let Ok(event) = event {
                    // Extract mission_id from event if present
                    let mission_id = match &event {
                        AgentEvent::ToolCall { mission_id, .. } => *mission_id,
                        AgentEvent::ToolResult { mission_id, .. } => *mission_id,
                        AgentEvent::Thinking { mission_id, .. } => *mission_id,
                        AgentEvent::TextDelta { mission_id, .. } => *mission_id,
                        AgentEvent::UserMessage { mission_id, .. } => *mission_id,
                        AgentEvent::AssistantMessage { mission_id, .. } => *mission_id,
                        AgentEvent::Error { mission_id, .. } => *mission_id,
                        AgentEvent::MissionStatusChanged { mission_id, .. } => Some(*mission_id),
                        AgentEvent::AgentPhase { mission_id, .. } => *mission_id,
                        AgentEvent::AgentTree { mission_id, .. } => *mission_id,
                        AgentEvent::Progress { mission_id, .. } => *mission_id,
                        AgentEvent::MissionActivity { mission_id, .. } => *mission_id,
                        AgentEvent::SessionIdUpdate { mission_id, .. } => Some(*mission_id),
                        AgentEvent::GoalIteration { mission_id, .. } => *mission_id,
                        AgentEvent::GoalStatus { mission_id, .. } => *mission_id,
                        _ => None,
                    };
                    // Update last_activity for matching runner (main or parallel)
                    if let Some(mid) = mission_id {
                        if running_mission_id == Some(mid) {
                            // Update main runner activity
                            main_runner_last_activity = std::time::Instant::now();
                        } else if let Some(runner) = parallel_runners.get_mut(&mid) {
                            // Update parallel runner activity
                            runner.touch();
                        }
                    }

                    // --- Activity tracking & subtask detection ---
                    match &event {
                        AgentEvent::ToolCall { name, args, tool_call_id, mission_id } => {
                            if let Some(mid) = mission_id {
                                let label = activity_label_from_tool_call(name, args);

                                // Update activity on runner
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = Some(label.clone());
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = Some(label.clone());
                                }

                                // Emit activity event for real-time SSE
                                let _ = events_tx.send(AgentEvent::MissionActivity {
                                    label,
                                    tool_name: name.clone(),
                                    mission_id: Some(*mid),
                                });

                                // Subtask detection
                                let is_subtask = matches!(name.as_str(),
                                    "Task" | "delegate_task" | "TaskCreate" | "Skill"
                                );
                                if is_subtask {
                                    let desc: String = args.get("description")
                                        .or_else(|| args.get("subject"))
                                        .or_else(|| args.get("prompt"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Subtask")
                                        .chars().take(120).collect();
                                    let info = super::mission_runner::SubtaskInfo {
                                        tool_call_id: tool_call_id.clone(),
                                        description: desc,
                                        completed: false,
                                    };
                                    let (total, completed) = if running_mission_id == Some(*mid) {
                                        main_runner_subtasks.push(info);
                                        (main_runner_subtasks.len(), main_runner_subtasks.iter().filter(|s| s.completed).count())
                                    } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                        runner.subtasks.push(info);
                                        (runner.subtasks.len(), runner.subtasks.iter().filter(|s| s.completed).count())
                                    } else {
                                        (0, 0)
                                    };
                                    if total > 0 {
                                        let _ = events_tx.send(AgentEvent::Progress {
                                            total_subtasks: total,
                                            completed_subtasks: completed,
                                            current_subtask: None,
                                            depth: 0,
                                            mission_id: Some(*mid),
                                        });
                                    }
                                }

                                // Desktop session detection from ToolCall.
                                // Claude Code and Amp don't emit ToolResult for MCP tools,
                                // so we detect the session start from the ToolCall and
                                // spawn a background task to attribute Xvfb processes.
                                let is_desktop_start = matches!(
                                    name.as_str(),
                                    "desktop_start_session"
                                        | "desktop_desktop_start_session"
                                        | "mcp__desktop__desktop_start_session"
                                );
                                if is_desktop_start {
                                    let store = mission_store.clone();
                                    let mid = *mid;
                                    tokio::spawn(async move {
                                        // Wait for Xvfb to start
                                        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

                                        // Scan for running Xvfb displays
                                        let displays =
                                            super::desktop::get_running_xvfb_displays().await;
                                        if displays.is_empty() {
                                            tracing::debug!(
                                                "No Xvfb displays found for desktop attribution"
                                            );
                                            return;
                                        }

                                        // Load current mission sessions
                                        let Ok(Some(mission)) = store.get_mission(mid).await
                                        else {
                                            return;
                                        };
                                        let mut sessions = mission.desktop_sessions.clone();
                                        let tracked: std::collections::HashSet<String> =
                                            sessions.iter().map(|s| s.display.clone()).collect();

                                        let mut changed = false;
                                        for disp in displays {
                                            if !tracked.contains(&disp) {
                                                sessions.push(DesktopSessionInfo {
                                                    display: disp.clone(),
                                                    resolution: None,
                                                    started_at: now_string(),
                                                    stopped_at: None,
                                                    screenshots_dir: None,
                                                    browser: None,
                                                    url: None,
                                                    mission_id: Some(mid),
                                                    keep_alive_until: None,
                                                });
                                                changed = true;
                                                tracing::info!(
                                                    display_id = %disp,
                                                    mission_id = %mid,
                                                    "Desktop session attributed from ToolCall"
                                                );
                                            }
                                        }

                                        if changed {
                                            if let Err(err) = store
                                                .update_mission_desktop_sessions(mid, &sessions)
                                                .await
                                            {
                                                tracing::warn!(
                                                    "Failed to persist desktop session from ToolCall for mission {}: {}",
                                                    mid,
                                                    err
                                                );
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        AgentEvent::ToolResult { tool_call_id, mission_id, .. } => {
                            if let Some(mid) = mission_id {
                                // Clear activity label (tool finished)
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = None;
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = None;
                                }

                                // Mark subtask complete if applicable
                                let subtasks: Option<&mut Vec<super::mission_runner::SubtaskInfo>> =
                                    if running_mission_id == Some(*mid) {
                                        Some(&mut main_runner_subtasks)
                                    } else {
                                        parallel_runners.get_mut(mid).map(|r| &mut r.subtasks)
                                    };
                                if let Some(subtasks) = subtasks {
                                    let mut changed = false;
                                    for s in subtasks.iter_mut() {
                                        if s.tool_call_id == *tool_call_id && !s.completed {
                                            s.completed = true;
                                            changed = true;
                                            break;
                                        }
                                    }
                                    if changed {
                                        let total = subtasks.len();
                                        let completed = subtasks.iter().filter(|s| s.completed).count();
                                        let _ = events_tx.send(AgentEvent::Progress {
                                            total_subtasks: total,
                                            completed_subtasks: completed,
                                            current_subtask: None,
                                            depth: 0,
                                            mission_id: Some(*mid),
                                        });
                                    }
                                }
                            }
                        }
                        AgentEvent::Thinking { done, mission_id, .. } => {
                            if let Some(mid) = mission_id {
                                let label = if *done { None } else { Some("Thinking…".to_string()) };
                                if running_mission_id == Some(*mid) {
                                    main_runner_activity = label;
                                } else if let Some(runner) = parallel_runners.get_mut(mid) {
                                    runner.current_activity = label;
                                }
                            }
                        }
                        _ => {}
                    }

                    // Track desktop sessions for mission reconnect/resume.
                    if let AgentEvent::ToolResult { name, result, mission_id, .. } = &event {
                        let Some(mid) = mission_id else {
                            continue;
                        };

                        let tool_name = name.as_str();
                        let is_start = matches!(
                            tool_name,
                            "desktop_start_session"
                                | "desktop_desktop_start_session"
                                | "mcp__desktop__desktop_start_session"
                        );
                        let is_stop = matches!(
                            tool_name,
                            "desktop_stop_session"
                                | "desktop_close_session"
                                | "desktop_desktop_stop_session"
                                | "desktop_desktop_close_session"
                                | "mcp__desktop__desktop_stop_session"
                                | "mcp__desktop__desktop_close_session"
                        );

                        if !is_start && !is_stop {
                            continue;
                        }

                        let Some(obj) = parse_tool_result_object(result) else {
                            continue;
                        };

                        let Some(display) = obj
                            .get("display")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string())
                        else {
                            continue;
                        };

                        let Ok(Some(mission)) = mission_store.get_mission(*mid).await else {
                            continue;
                        };

                        let mut sessions = mission.desktop_sessions.clone();
                        let now = now_string();

                        if is_start {
                            let resolution = obj
                                .get("resolution")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let screenshots_dir = obj
                                .get("screenshots_dir")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let browser = obj
                                .get("browser")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                            let url = obj
                                .get("url")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());

                            if let Some(existing) = sessions
                                .iter_mut()
                                .rev()
                                .find(|session| session.display == display && session.stopped_at.is_none())
                            {
                                existing.resolution = resolution;
                                existing.screenshots_dir = screenshots_dir;
                                existing.browser = browser;
                                existing.url = url;
                                existing.started_at = now.clone();
                            } else {
                                sessions.push(DesktopSessionInfo {
                                    display,
                                    resolution,
                                    started_at: now.clone(),
                                    stopped_at: None,
                                    screenshots_dir,
                                    browser,
                                    url,
                                    mission_id: Some(*mid),
                                    keep_alive_until: None,
                                });
                            }
                        } else if let Some(existing) = sessions
                            .iter_mut()
                            .rev()
                            .find(|session| session.display == display && session.stopped_at.is_none())
                        {
                            existing.stopped_at = Some(now.clone());
                        }

                        if let Err(err) = mission_store
                            .update_mission_desktop_sessions(*mid, &sessions)
                            .await
                        {
                            tracing::warn!(
                                "Failed to persist desktop session info for mission {}: {}",
                                mid,
                                err
                            );
                        }
                    }

                    // Handle session ID updates (for backends like Amp that generate their own IDs)
                    if let AgentEvent::SessionIdUpdate { mission_id, session_id } = &event {
                        if let Err(err) = mission_store
                            .update_mission_session_id(*mission_id, session_id)
                            .await
                        {
                            tracing::warn!(
                                "Failed to update session ID for mission {}: {}",
                                mission_id,
                                err
                            );
                        } else {
                            tracing::debug!(
                                mission_id = %mission_id,
                                session_id = %session_id,
                                "Updated mission session ID from backend"
                            );
                        }
                        // Also update the parallel runner's cached session_id so the
                        // next turn picks up the new value instead of the stale one.
                        if let Some(runner) = parallel_runners.get_mut(mission_id) {
                            runner.session_id = Some(session_id.clone());
                        }
                    }

                    // Persist `/goal` metadata so refreshes and restart recovery
                    // can continue through codex goal mode instead of a plain turn.
                    if let AgentEvent::UserMessage {
                        content,
                        mission_id: Some(mid),
                        ..
                    } = &event
                    {
                        if let Some(objective) = parse_goal_objective(content) {
                            if let Err(err) = mission_store
                                .update_mission_goal(*mid, true, Some(&objective))
                                .await
                            {
                                tracing::warn!(
                                    mission_id = %mid,
                                    "Failed to persist goal metadata from user message: {}",
                                    err
                                );
                            }
                        }
                    }

                    if let AgentEvent::GoalStatus {
                        status,
                        objective,
                        mission_id: Some(mid),
                    } = &event
                    {
                        let goal_mode = status != "cleared";
                        let objective = goal_mode.then_some(objective.as_str());
                        if let Err(err) = mission_store
                            .update_mission_goal(*mid, goal_mode, objective)
                            .await
                        {
                            tracing::warn!(
                                mission_id = %mid,
                                status = %status,
                                "Failed to persist goal metadata from goal status: {}",
                                err
                            );
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_single_control_turn(
    mut config: Config,
    _root_agent: AgentRef,
    mcp: Arc<McpRegistry>,
    workspaces: workspace::SharedWorkspaceStore,
    library: SharedLibrary,
    events_tx: broadcast::Sender<AgentEvent>,
    tool_hub: Arc<FrontendToolHub>,
    status: Arc<RwLock<ControlStatus>>,
    cancel: CancellationToken,
    history: Vec<(String, String)>,
    user_message: String,
    mission_control: Option<crate::tools::mission::MissionControl>,
    tree_snapshot: Arc<RwLock<Option<AgentTreeNode>>>,
    progress_snapshot: Arc<RwLock<ExecutionProgress>>,
    mission_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    backend_id: Option<String>,
    model_override: Option<String>,
    model_effort: Option<String>,
    agent_override: Option<String>,
    session_id: Option<String>,
    force_session_resume: bool,
    mission_config_profile: Option<String>,
) -> crate::agents::AgentResult {
    let is_claudecode = backend_id.as_deref() == Some("claudecode");
    // Get config profile: mission's config_profile takes priority over workspace's
    let workspace_config_profile = if let Some(ws_id) = workspace_id {
        workspaces.get(ws_id).await.and_then(|ws| ws.config_profile)
    } else {
        None
    };
    let effective_config_profile = mission_config_profile.or(workspace_config_profile);
    let requested_model = model_override;
    let requested_model_effort = model_effort;
    if let Some(ref model) = requested_model {
        config.default_model = Some(model.clone());
    } else if is_claudecode && config.default_model.is_none() {
        if let Some(default_model) =
            resolve_claudecode_default_model(&library, effective_config_profile.as_deref()).await
        {
            config.default_model = Some(default_model);
        }
    } else if (backend_id.as_deref() == Some("opencode")
        && effective_config_profile.is_some()
        && requested_model.is_none())
        || (backend_id.as_deref() == Some("codex") && requested_model.is_none())
    {
        config.default_model = None;
    } else if backend_id.as_deref() == Some("gemini") && requested_model.is_none() {
        config.default_model = Some(resolve_gemini_default_model());
    }
    if let Some(ref agent) = agent_override {
        config.opencode_agent = Some(agent.clone());
    }
    // Ensure a workspace directory for this mission (if applicable).
    let (working_dir_path, runtime_workspace) = if let Some(mid) = mission_id {
        let ws = workspace::resolve_workspace(&workspaces, &config, workspace_id).await;
        if let Err(e) =
            workspace::sync_workspace_mcp_binaries_for_workspace(&config.working_dir, &ws).await
        {
            tracing::warn!(
                workspace = %ws.name,
                error = %e,
                "Failed to sync MCP binaries into workspace"
            );
        }
        // Get library for skill syncing
        let lib_guard = library.read().await;
        let lib_ref = lib_guard.as_ref().map(|l| l.as_ref());
        let dir = match Box::pin(workspace::prepare_mission_workspace_with_skills_backend(
            &ws,
            &mcp,
            lib_ref,
            mid,
            backend_id.as_deref().unwrap_or("opencode"),
            None, // custom_providers: TODO integrate with provider store
            effective_config_profile.as_deref(),
        ))
        .await
        {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!("Failed to prepare mission workspace: {}", e);
                ws.path.clone()
            }
        };
        (dir, Some(ws))
    } else {
        (
            config.working_dir.clone(),
            Some(workspace::Workspace::default_host(
                config.working_dir.clone(),
            )),
        )
    };

    if let Some(ws) = runtime_workspace.as_ref() {
        if let Err(e) = Box::pin(workspace::write_runtime_workspace_state(
            &config.working_dir,
            ws,
            &working_dir_path,
            mission_id,
            &config.context.context_dir_name,
        ))
        .await
        {
            tracing::warn!("Failed to write runtime workspace state: {}", e);
        }
    }

    // For Telegram missions, append channel instructions and memory awareness
    // to CLAUDE.md so the backend LLM adopts the bot persona.
    if user_message.contains("[Telegram from ") {
        let claude_md_path = working_dir_path.join("CLAUDE.md");
        tracing::info!(
            mission_id = ?mission_id,
            claude_md_path = %claude_md_path.display(),
            claude_md_exists = claude_md_path.exists(),
            "Telegram message detected in control path, attempting CLAUDE.md injection"
        );
        // Create the file if it doesn't exist so that non-Claude-Code
        // backends (e.g. opencode) also get the identity injection.
        if !claude_md_path.exists() {
            let _ = std::fs::write(&claude_md_path, "");
        }
        let actions_available = mission_id
            .and_then(crate::api::telegram::build_internal_telegram_action_token)
            .is_some()
            && super::mission_runner::localhost_api_base_url_from_env().is_some();
        super::mission_runner::inject_telegram_identity_into_claude_md(
            &claude_md_path,
            &user_message,
            actions_available,
        );
    }

    // Build a task prompt that includes conversation context with size limits.
    let history_for_prompt = match history.last() {
        Some((role, content)) if role == "user" && content == &user_message => {
            &history[..history.len() - 1]
        }
        _ => history.as_slice(),
    };
    let history_context =
        build_history_context(history_for_prompt, config.context.max_history_total_chars);
    let mut convo = String::new();
    convo.push_str(&history_context);
    convo.push_str("User:\n");
    convo.push_str(&user_message);
    convo.push_str("\n\nInstructions:\n- Continue the conversation helpfully.\n- Use available tools as needed.\n- For large data processing tasks (>10KB), prefer executing scripts rather than inline processing.\n");
    let _task = match crate::task::Task::new(convo.clone(), Some(1000)) {
        Ok(t) => t,
        Err(e) => {
            let r = crate::agents::AgentResult::failure(format!("Failed to create task: {}", e), 0);
            return r;
        }
    };

    // Context for agent execution.
    let mut ctx = AgentContext::new(config.clone(), working_dir_path);
    ctx.mission_control = mission_control;
    ctx.control_events = Some(events_tx.clone());
    ctx.frontend_tool_hub = Some(tool_hub.clone());
    ctx.control_status = Some(status.clone());
    ctx.cancel_token = Some(cancel.clone());
    ctx.tree_snapshot = Some(tree_snapshot);
    ctx.progress_snapshot = Some(progress_snapshot);
    ctx.mission_id = mission_id;
    ctx.mcp = Some(mcp);

    let fallback_workspace = workspace::Workspace::default_host(config.working_dir.clone());
    let exec_workspace = runtime_workspace.as_ref().unwrap_or(&fallback_workspace);

    // Execute based on backend
    let result = match backend_id.as_deref() {
        Some("claudecode") => {
            let mid = match require_mission_id(mission_id, "Claude Code", &events_tx) {
                Ok(id) => id,
                Err(r) => return r,
            };
            // Check if this is a continuation turn (has prior assistant response).
            // Note: history may include the current user message before the turn runs,
            // so we check for assistant messages to determine if this is truly a continuation.
            // Also use --resume if force_session_resume is set (e.g., for mission resume operations
            // where the session exists but history may not have assistant messages yet).
            let is_continuation =
                force_session_resume || history.iter().any(|(role, _)| role == "assistant");
            let mut effective_message = user_message.clone();
            let mut effective_session_id = session_id.clone();
            let mut attempted_same_session_resume = false;
            let mut attempted_session_reset = false;
            let mut result = Box::pin(super::mission_runner::run_claudecode_turn(
                exec_workspace,
                &ctx.working_dir,
                &effective_message,
                config.default_model.as_deref(),
                requested_model_effort.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel.clone(),
                None, // secrets - not available in control context
                &config.working_dir,
                effective_session_id.as_deref(),
                is_continuation,
                Some(tool_hub.clone()),
                Some(status.clone()),
                None, // override_auth
            ))
            .await;

            loop {
                match super::mission_runner::claudecode_transport_recovery_strategy(
                    &result,
                    effective_session_id.is_some(),
                    attempted_same_session_resume,
                    attempted_session_reset,
                ) {
                    super::mission_runner::ClaudeTransportRecoveryStrategy::None => break,
                    super::mission_runner::ClaudeTransportRecoveryStrategy::ResumeCurrentSession => {
                        attempted_same_session_resume = true;
                        tracing::warn!(
                            mission_id = %mid,
                            session_id = ?effective_session_id,
                            error = %result.output,
                            "Incomplete Claude turn detected; retrying once by continuing the current session"
                        );
                        effective_message =
                            super::mission_runner::claudecode_resume_current_session_message()
                                .to_string();
                        result = Box::pin(super::mission_runner::run_claudecode_turn(
                            exec_workspace,
                            &ctx.working_dir,
                            &effective_message,
                            config.default_model.as_deref(),
                            requested_model_effort.as_deref(),
                            config.opencode_agent.as_deref(),
                            mid,
                            events_tx.clone(),
                            cancel.clone(),
                            None,
                            &config.working_dir,
                            effective_session_id.as_deref(),
                            true,
                            Some(tool_hub.clone()),
                            Some(status.clone()),
                            None,
                        ))
                        .await;
                    }
                    super::mission_runner::ClaudeTransportRecoveryStrategy::ResetSessionFresh => {
                        attempted_session_reset = true;
                        let new_session_id = Uuid::new_v4().to_string();
                        tracing::warn!(
                            mission_id = %mid,
                            old_session_id = ?effective_session_id,
                            new_session_id = %new_session_id,
                            attempted_same_session_resume,
                            error = %result.output,
                            "Session corruption detected; resetting session and retrying once"
                        );

                        let _ = events_tx.send(AgentEvent::SessionIdUpdate {
                            mission_id: mid,
                            session_id: new_session_id.clone(),
                        });

                        let session_marker = ctx.working_dir.join(".claude-session-initiated");
                        if session_marker.exists() {
                            let _ = std::fs::remove_file(&session_marker);
                        }

                        let history_for_retry = match history.last() {
                            Some((role, content)) if role == "user" && content == &user_message => {
                                &history[..history.len() - 1]
                            }
                            _ => history.as_slice(),
                        };
                        let retry_message = if history_for_retry.is_empty() {
                            user_message.clone()
                        } else {
                            let history_ctx = build_history_context(
                                history_for_retry,
                                config.context.max_history_total_chars,
                            );
                            format!(
                                "## Prior conversation (session was reset due to a transient error)\n\n\
                                 {history_ctx}\
                                 ## Current message\n\n\
                                 {user_message}"
                            )
                        };

                        effective_message = retry_message;
                        effective_session_id = Some(new_session_id);

                        result = Box::pin(super::mission_runner::run_claudecode_turn(
                            exec_workspace,
                            &ctx.working_dir,
                            &effective_message,
                            config.default_model.as_deref(),
                            requested_model_effort.as_deref(),
                            config.opencode_agent.as_deref(),
                            mid,
                            events_tx.clone(),
                            cancel.clone(),
                            None,
                            &config.working_dir,
                            effective_session_id.as_deref(),
                            false,
                            Some(tool_hub.clone()),
                            Some(status.clone()),
                            None,
                        ))
                        .await;
                    }
                }
            }

            // Auth error recovery: if the token was revoked server-side but the
            // local expiry hadn't passed yet, invalidate stale credentials, force
            // an OAuth refresh, and retry once.
            if result.terminal_reason == Some(TerminalReason::AuthError) && !cancel.is_cancelled() {
                tracing::warn!(
                    mission_id = %mid,
                    "Auth error detected — invalidating stale credentials and retrying"
                );

                super::mission_runner::refresh_claude_credentials_after_auth_error(
                    &ctx.working_dir,
                    "control_initial_auth_error",
                )
                .await;

                // Retry with fresh credentials
                result = Box::pin(super::mission_runner::run_claudecode_turn(
                    exec_workspace,
                    &ctx.working_dir,
                    &effective_message,
                    config.default_model.as_deref(),
                    requested_model_effort.as_deref(),
                    config.opencode_agent.as_deref(),
                    mid,
                    events_tx.clone(),
                    cancel.clone(),
                    None,
                    &config.working_dir,
                    effective_session_id.as_deref(),
                    is_continuation,
                    Some(tool_hub.clone()),
                    Some(status.clone()),
                    None,
                ))
                .await;
            }

            // Account rotation: if rate-limited, try alternate Anthropic credentials.
            // The first entry in the list is the highest-priority credential, which
            // is almost certainly what the initial (override_auth=None) call used.
            // Skip it to avoid a guaranteed duplicate rate-limit failure.
            let mut rotated_anthropic_account = false;
            if result.terminal_reason == Some(TerminalReason::RateLimited) && !cancel.is_cancelled()
            {
                let rotation_accounts = super::mission_runner::anthropic_rotation_accounts(
                    exec_workspace,
                    &ctx.working_dir,
                    &config.working_dir,
                );
                if !rotation_accounts.accounts.is_empty() {
                    tracing::info!(
                        mission_id = %mid,
                        total_accounts = rotation_accounts.total_accounts,
                        alternate_accounts = rotation_accounts.accounts.len(),
                        skipped_current = rotation_accounts.skipped_current,
                        "Rate limited on primary account; trying alternate Anthropic credentials"
                    );
                    for (idx, alt_auth) in rotation_accounts.accounts.into_iter().enumerate() {
                        if cancel.is_cancelled() {
                            break;
                        }
                        rotated_anthropic_account = true;
                        tracing::info!(
                            mission_id = %mid,
                            rotation_attempt = idx + 1,
                            auth_type = match &alt_auth {
                                super::ai_providers::ClaudeCodeAuth::ApiKey(_) => "api_key",
                                super::ai_providers::ClaudeCodeAuth::OAuthToken(_) =>
                                    "oauth_token",
                            },
                            "Rotating to alternate Anthropic account"
                        );
                        result = Box::pin(super::mission_runner::run_claudecode_turn(
                            exec_workspace,
                            &ctx.working_dir,
                            &effective_message,
                            config.default_model.as_deref(),
                            requested_model_effort.as_deref(),
                            config.opencode_agent.as_deref(),
                            mid,
                            events_tx.clone(),
                            cancel.clone(),
                            None,
                            &config.working_dir,
                            effective_session_id.as_deref(),
                            is_continuation,
                            Some(tool_hub.clone()),
                            Some(status.clone()),
                            Some(alt_auth),
                        ))
                        .await;
                        // Only continue rotating on rate-limit errors.
                        match result.terminal_reason {
                            Some(TerminalReason::RateLimited) => {
                                tracing::info!(
                                    mission_id = %mid,
                                    rotation_attempt = idx + 1,
                                    "Rate limited; rotating to next account"
                                );
                                continue;
                            }
                            _ => break,
                        }
                    }
                }
            }

            // Account rotation can surface a revoked/expired alternate OAuth
            // credential. Run the same stale-credential recovery after rotation
            // so the mission retries with freshly refreshed host credentials
            // instead of stopping on "Invalid authentication credentials".
            if rotated_anthropic_account
                && result.terminal_reason == Some(TerminalReason::AuthError)
                && !cancel.is_cancelled()
            {
                tracing::warn!(
                    mission_id = %mid,
                    "Auth error detected after credential rotation - invalidating stale credentials and retrying"
                );

                super::mission_runner::refresh_claude_credentials_after_auth_error(
                    &ctx.working_dir,
                    "control_rotated_auth_error",
                )
                .await;

                result = Box::pin(super::mission_runner::run_claudecode_turn(
                    exec_workspace,
                    &ctx.working_dir,
                    &effective_message,
                    config.default_model.as_deref(),
                    requested_model_effort.as_deref(),
                    config.opencode_agent.as_deref(),
                    mid,
                    events_tx.clone(),
                    cancel.clone(),
                    None,
                    &config.working_dir,
                    effective_session_id.as_deref(),
                    is_continuation,
                    Some(tool_hub.clone()),
                    Some(status.clone()),
                    None,
                ))
                .await;
            }

            result
        }
        Some("amp") => {
            let mid = match require_mission_id(mission_id, "Amp", &events_tx) {
                Ok(id) => id,
                Err(r) => return r,
            };
            let is_continuation =
                force_session_resume || history.iter().any(|(role, _)| role == "assistant");
            let api_key = super::mission_runner::get_amp_api_key_from_config();
            Box::pin(super::mission_runner::run_amp_turn(
                exec_workspace,
                &ctx.working_dir,
                &user_message,
                config.opencode_agent.as_deref(), // mode (smart/rush)
                mid,
                events_tx.clone(),
                cancel,
                &config.working_dir,
                session_id.as_deref(),
                is_continuation,
                api_key.as_deref(),
            ))
            .await
        }
        Some("codex") => {
            let mid = match require_mission_id(mission_id, "Codex", &events_tx) {
                Ok(id) => id,
                Err(r) => return r,
            };
            let requested_codex_model = requested_model
                .as_deref()
                .or(config.default_model.as_deref());
            // Goal-mode missions need the raw `/goal <objective>` message to
            // reach the codex backend; the wrapped `convo` buries the prefix
            // and breaks `parse_goal_prefix`. Mirror the same routing the
            // mission_runner dispatch uses.
            let codex_message_owned: String = if user_message.trim_start().starts_with("/goal ") {
                user_message.clone()
            } else {
                convo.clone()
            };
            let codex_message: &str = codex_message_owned.as_str();
            let mut result = Box::pin(super::mission_runner::run_codex_turn(
                exec_workspace,
                &ctx.working_dir,
                codex_message,
                requested_codex_model,
                requested_model_effort.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel.clone(),
                &config.working_dir,
                session_id.as_deref(),
                None,
            ))
            .await;

            if let Some(fallback_model) = super::mission_runner::codex_chatgpt_fallback_for_result(
                requested_codex_model,
                &result,
            ) {
                tracing::warn!(
                    mission_id = %mid,
                    requested_model = ?requested_codex_model,
                    fallback_model,
                    "Retrying Codex turn with fallback model for ChatGPT account compatibility (control path)"
                );
                result = Box::pin(super::mission_runner::run_codex_turn(
                    exec_workspace,
                    &ctx.working_dir,
                    codex_message,
                    Some(fallback_model),
                    requested_model_effort.as_deref(),
                    config.opencode_agent.as_deref(),
                    mid,
                    events_tx.clone(),
                    cancel,
                    &config.working_dir,
                    session_id.as_deref(),
                    None,
                ))
                .await;
            } else if super::mission_runner::codex_tool_stall_should_retry_with_default_model(
                requested_codex_model,
                &result,
            ) {
                tracing::warn!(
                    mission_id = %mid,
                    requested_model = ?requested_codex_model,
                    "Retrying Codex turn with CLI default model after generic GPT model stopped before tool use (control path)"
                );
                result = Box::pin(super::mission_runner::run_codex_turn(
                    exec_workspace,
                    &ctx.working_dir,
                    codex_message,
                    None,
                    requested_model_effort.as_deref(),
                    config.opencode_agent.as_deref(),
                    mid,
                    events_tx.clone(),
                    cancel,
                    &config.working_dir,
                    session_id.as_deref(),
                    None,
                ))
                .await;
            }

            result
        }
        Some("gemini") => {
            let mid = match require_mission_id(mission_id, "Gemini CLI", &events_tx) {
                Ok(id) => id,
                Err(r) => return r,
            };
            Box::pin(super::mission_runner::run_gemini_turn(
                exec_workspace,
                &ctx.working_dir,
                &convo,
                config.default_model.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel,
                &config.working_dir,
                session_id.as_deref(),
            ))
            .await
        }
        Some(backend) if backend != "opencode" => {
            let _ = events_tx.send(AgentEvent::Error {
                message: format!("Unsupported backend: {}", backend),
                mission_id,
                resumable: mission_id.is_some(),
            });
            crate::agents::AgentResult::failure(format!("Unsupported backend: {}", backend), 0)
                .with_terminal_reason(TerminalReason::LlmError)
        }
        _ => {
            // Default to opencode using per-workspace CLI execution
            let mid = mission_id.unwrap_or_else(Uuid::nil);
            // Check profile's sandboxed config for disable_oh_my_opencode flag
            let mut opencode_workspace = exec_workspace.clone();
            if let Some(ref profile) = effective_config_profile {
                let lib_guard = library.read().await;
                if let Some(lib) = lib_guard.as_ref() {
                    if let Ok(profile_data) = lib.get_config_profile(profile).await {
                        if profile_data.sandboxed_config.disable_oh_my_opencode {
                            tracing::info!(
                                mission_id = ?mission_id,
                                profile = %profile,
                                "Enabling plain opencode mode from config profile"
                            );
                            let mut obj = opencode_workspace
                                .config
                                .as_object()
                                .cloned()
                                .unwrap_or_default();
                            obj.insert(
                                "disable_oh_my_opencode".to_string(),
                                serde_json::json!(true),
                            );
                            opencode_workspace.config = serde_json::Value::Object(obj);
                        }
                    }
                }
            }
            Box::pin(super::mission_runner::run_opencode_turn(
                &opencode_workspace,
                &ctx.working_dir,
                &user_message,
                config.default_model.as_deref(),
                requested_model_effort.as_deref(),
                config.opencode_agent.as_deref(),
                mid,
                events_tx.clone(),
                cancel,
                &config.working_dir,
            ))
            .await
        }
    };
    result
}

// === Automation API handlers ===

#[derive(Debug, Deserialize)]
pub struct CreateAutomationRequest {
    pub command_source: mission_store::CommandSource,
    pub trigger: mission_store::TriggerType,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    #[serde(default)]
    pub retry_config: Option<mission_store::RetryConfig>,
    #[serde(default)]
    pub stop_policy: Option<mission_store::StopPolicy>,
    #[serde(default)]
    pub fresh_session: Option<mission_store::FreshSession>,
    /// When true, trigger the first execution immediately after creation.
    #[serde(default)]
    pub start_immediately: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAutomationRequest {
    pub command_source: Option<mission_store::CommandSource>,
    pub trigger: Option<mission_store::TriggerType>,
    pub variables: Option<HashMap<String, String>>,
    pub retry_config: Option<mission_store::RetryConfig>,
    pub stop_policy: Option<mission_store::StopPolicy>,
    pub fresh_session: Option<mission_store::FreshSession>,
    pub active: Option<bool>,
}

/// List all automations for a mission.
pub async fn list_mission_automations(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Vec<mission_store::Automation>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let automations = control
        .mission_store
        .get_mission_automations(mission_id)
        .await
        .map_err(internal_error)?;

    Ok(Json(automations))
}

/// List all active automations across missions.
pub async fn list_active_automations(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<mission_store::Automation>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let automations = control
        .mission_store
        .list_active_automations()
        .await
        .map_err(internal_error)?;

    Ok(Json(automations))
}

/// Create an automation for a mission.
pub async fn create_automation(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<CreateAutomationRequest>,
) -> Result<Json<mission_store::Automation>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    // Validate the command exists in the library if CommandSource::Library
    if let mission_store::CommandSource::Library { ref name } = req.command_source {
        validate_library_command(&state, name).await?;
    }

    // Generate webhook_id if trigger type is Webhook
    let trigger = match req.trigger {
        mission_store::TriggerType::Webhook { mut config } => {
            // Generate a unique webhook_id if not provided or empty
            if config.webhook_id.is_empty() {
                config.webhook_id = Uuid::new_v4().to_string();
            }
            mission_store::TriggerType::Webhook { config }
        }
        other => other,
    };

    // Validate cron expression before persisting
    if let mission_store::TriggerType::Cron { ref expression, .. } = trigger {
        if croner::Cron::new(expression).parse().is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid cron expression: {}", expression),
            ));
        }
    }

    let start_immediately = req.start_immediately;

    // For interval/cron triggers, if start_immediately is false, set last_triggered_at
    // to now so the scheduler waits for the next occurrence before the first trigger.
    let last_triggered_at = if !start_immediately
        && matches!(
            trigger,
            mission_store::TriggerType::Interval { .. } | mission_store::TriggerType::Cron { .. }
        ) {
        Some(mission_store::now_string())
    } else {
        None
    };

    // Build the complete Automation struct
    let fresh_session = req
        .fresh_session
        .unwrap_or(mission_store::FreshSession::Keep);
    if fresh_session == mission_store::FreshSession::Switch {
        let has_next_session_id = req
            .variables
            .get("nextSessionId")
            .map(|value| Uuid::parse_str(value.trim()).is_ok())
            .unwrap_or(false);
        if !has_next_session_id {
            return Err((
                StatusCode::BAD_REQUEST,
                "session mode 'switch' requires variables.nextSessionId to be a valid mission UUID"
                    .to_string(),
            ));
        }
    }

    let automation = mission_store::Automation {
        id: Uuid::new_v4(),
        mission_id,
        command_source: req.command_source,
        trigger,
        variables: req.variables,
        active: true,
        stop_policy: req
            .stop_policy
            .unwrap_or(mission_store::StopPolicy::WhenFailingConsecutively { count: 2 }),
        fresh_session,
        created_at: mission_store::now_string(),
        last_triggered_at,
        retry_config: req.retry_config.unwrap_or_default(),
        consecutive_failures: 0,
    };

    let mut automation = control
        .mission_store
        .create_automation(automation)
        .await
        .map_err(internal_error)?;

    // If start_immediately is requested for agent_finished triggers, fire the
    // first execution right away by resolving the command and sending it as a
    // user message to the control actor.
    if start_immediately
        && matches!(
            automation.trigger,
            mission_store::TriggerType::AgentFinished
        )
    {
        if let Ok(Some(mission)) = control.mission_store.get_mission(mission_id).await {
            // Newly created automation has 0 consecutive failures
            if stop_policy_matches_status(&automation.stop_policy, mission.status, 0).await {
                let mut updated = automation.clone();
                updated.active = false;
                if let Err(e) = control.mission_store.update_automation(updated).await {
                    tracing::warn!(
                        "Failed to disable automation {} on create due to stop policy: {}",
                        automation.id,
                        e
                    );
                }
                automation.active = false;
                return Ok(Json(automation));
            }
        }

        let cmd_content =
            resolve_automation_command(&automation, mission_id, &state, &control.mission_store)
                .await;

        if let Some(content) = cmd_content {
            let target_mission_id = match control.mission_store.get_mission(mission_id).await {
                Ok(Some(mission)) => {
                    resolve_agent_finished_target_mission(mission_id, mission.status, &automation)
                }
                Ok(None) => mission_id,
                Err(e) => {
                    tracing::warn!(
                        "Failed to load mission {} while resolving start_immediately target: {}",
                        mission_id,
                        e
                    );
                    mission_id
                }
            };

            // Record the execution in Running status – it will be completed
            // by complete_running_executions_for_mission when the agent
            // finishes processing.
            let execution_id = Uuid::new_v4();
            let execution = mission_store::AutomationExecution {
                id: execution_id,
                automation_id: automation.id,
                mission_id: target_mission_id,
                triggered_at: mission_store::now_string(),
                trigger_source: "start_immediately".to_string(),
                status: mission_store::ExecutionStatus::Running,
                webhook_payload: None,
                variables_used: automation.variables.clone(),
                completed_at: None,
                error: None,
                retry_count: 0,
            };
            let _ = control
                .mission_store
                .create_automation_execution(execution)
                .await;
            let _ = control
                .mission_store
                .update_automation_last_triggered(automation.id)
                .await;

            // Send as a user message to the mission
            let (respond_tx, _respond_rx) = tokio::sync::oneshot::channel();
            let _ = control
                .cmd_tx
                .send(ControlCommand::UserMessage {
                    id: Uuid::new_v4(),
                    content,
                    agent: None,
                    target_mission_id: Some(target_mission_id),
                    respond: respond_tx,
                })
                .await;
        }
    }

    Ok(Json(automation))
}

/// Get an automation by ID.
pub async fn get_automation(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(automation_id): Path<Uuid>,
) -> Result<Json<mission_store::Automation>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let automation = require_automation(&control.mission_store, automation_id).await?;

    Ok(Json(automation))
}

/// Update an automation.
pub async fn update_automation(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(automation_id): Path<Uuid>,
    Json(req): Json<UpdateAutomationRequest>,
) -> Result<Json<mission_store::Automation>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let mut automation = require_automation(&control.mission_store, automation_id).await?;

    // Validate the command exists in the library if CommandSource::Library is being updated
    if let Some(mission_store::CommandSource::Library { name }) = req.command_source.as_ref() {
        validate_library_command(&state, name).await?;
    }

    // Update fields if provided
    if let Some(command_source) = req.command_source {
        automation.command_source = command_source;
    }

    if let Some(trigger) = req.trigger {
        // Generate webhook_id if trigger type is Webhook and webhook_id is empty
        automation.trigger = match trigger {
            mission_store::TriggerType::Webhook { mut config } => {
                if config.webhook_id.is_empty() {
                    config.webhook_id = Uuid::new_v4().to_string();
                }
                mission_store::TriggerType::Webhook { config }
            }
            other => other,
        };
    }

    if let Some(variables) = req.variables {
        automation.variables = variables;
    }

    if let Some(retry_config) = req.retry_config {
        automation.retry_config = retry_config;
    }

    if let Some(stop_policy) = req.stop_policy {
        automation.stop_policy = stop_policy;
    }

    if let Some(fresh_session) = req.fresh_session {
        automation.fresh_session = fresh_session;
    }

    if let Some(active) = req.active {
        automation.active = active;
    }

    if automation.fresh_session == mission_store::FreshSession::Switch {
        let has_next_session_id = automation
            .variables
            .get("nextSessionId")
            .map(|value| Uuid::parse_str(value.trim()).is_ok())
            .unwrap_or(false);
        if !has_next_session_id {
            return Err((
                StatusCode::BAD_REQUEST,
                "session mode 'switch' requires variables.nextSessionId to be a valid mission UUID"
                    .to_string(),
            ));
        }
    }

    // Update automation in the store
    control
        .mission_store
        .update_automation(automation.clone())
        .await
        .map_err(internal_error)?;

    Ok(Json(automation))
}

/// Delete an automation.
pub async fn delete_automation(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(automation_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let deleted = control
        .mission_store
        .delete_automation(automation_id)
        .await
        .map_err(internal_error)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("Automation {} not found", automation_id),
        ))
    }
}

/// Get execution history for an automation.
pub async fn get_automation_executions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(automation_id): Path<Uuid>,
) -> Result<Json<Vec<mission_store::AutomationExecution>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let _automation = require_automation(&control.mission_store, automation_id).await?;

    let executions = control
        .mission_store
        .get_automation_executions(automation_id, Some(100))
        .await
        .map_err(internal_error)?;

    Ok(Json(executions))
}

/// Get all automation executions for a mission.
pub async fn get_mission_automation_executions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Vec<mission_store::AutomationExecution>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let executions = control
        .mission_store
        .get_mission_automation_executions(mission_id, Some(100))
        .await
        .map_err(internal_error)?;

    Ok(Json(executions))
}

/// Export a mission as a portable bundle for transfer to another instance.
///
/// Response: JSON by default, or gzipped JSON when the caller sends
/// `Accept-Encoding: gzip` (or explicitly forces it with `?gzip=true`).
/// Gzipping typically shrinks bundles 5–10× — a 220 MB text-heavy bundle
/// lands around 30 MB, comfortably under Cloudflare's 100 MB free-tier
/// request cap. Clients that can't decompress pass `?gzip=false` to
/// disable.
#[derive(Debug, Deserialize, Default)]
pub struct ExportMissionQuery {
    /// Force gzip encoding regardless of Accept-Encoding. `None` defers to
    /// the header; `Some(false)` disables even if Accept-Encoding asked.
    #[serde(default)]
    pub gzip: Option<bool>,
}

pub async fn export_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<ExportMissionQuery>,
    headers_in: axum::http::HeaderMap,
) -> Result<axum::response::Response, (StatusCode, String)> {
    use axum::http::header;
    use axum::response::IntoResponse;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let control = control_for_user(&state, &user).await;
    let source_public_url = std::env::var("SANDBOXED_PUBLIC_URL").ok();
    let mut bundle = control
        .mission_store
        .export_mission_bundle(mission_id, source_public_url)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    // Ensure `workspace_name` is populated so the import side can resolve
    // it against its own workspace store without requiring the caller to
    // pass `?workspace_id=`. The stored `missions.workspace_name` column
    // can be NULL (it's only populated opportunistically at display time).
    if bundle.workspace_name.is_none() {
        if let Some(ws) = state.workspaces.get(bundle.mission.workspace_id).await {
            bundle.workspace_name = Some(ws.name.clone());
            bundle.mission.workspace_name = Some(ws.name);
        }
    }
    let mission_id_simple = bundle.mission.id.simple().to_string();

    // Gzip opt-in:
    //   1. `?gzip=true` always compresses
    //   2. `?gzip=false` always skips
    //   3. Otherwise honor Accept-Encoding: gzip, including q-values
    //      (`gzip;q=0` explicitly disallows gzip even when present).
    let accepts_gzip = headers_in
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(accept_encoding_allows_gzip)
        .unwrap_or(false);
    let use_gzip = query.gzip.unwrap_or(accepts_gzip);

    // Stream the bundle out instead of materializing a full JSON string
    // (and a full gzip buffer on top of that). For large missions the
    // old approach held bundle + JSON + optional gzip — three in-memory
    // copies at once — which could OOM the daemon on export. Here the
    // serializer writes into a bounded mpsc channel in ~64 KB chunks;
    // flate2 compresses on the fly when use_gzip is set.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let mut writer = ChannelWriter::new(tx);
        let serialize_result: std::io::Result<()> = if use_gzip {
            let mut encoder = GzEncoder::new(&mut writer, Compression::default());
            serde_json::to_writer(&mut encoder, &bundle)
                .map_err(std::io::Error::other)
                .and_then(|_| encoder.finish().map(|_| ()))
        } else {
            serde_json::to_writer(&mut writer, &bundle).map_err(std::io::Error::other)
        };
        if let Err(e) = serialize_result.and_then(|_| writer.flush_all()) {
            writer.send_error(e);
        }
    });
    // Adapt the mpsc receiver into a `Stream` via futures::stream::unfold —
    // tokio-stream isn't in the dep set, and this is a ~5-line adapter.
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let body = axum::body::Body::from_stream(stream);

    let filename = if use_gzip {
        format!("mission-{mission_id_simple}.json.gz")
    } else {
        format!("mission-{mission_id_simple}.json")
    };

    let mut resp = (StatusCode::OK, body).into_response();
    let out_headers = resp.headers_mut();
    out_headers.insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    );
    if use_gzip {
        out_headers.insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        // Mark the response as varying on Accept-Encoding so caches
        // don't serve a gzipped body to a client that didn't ask.
        out_headers.insert(header::VARY, "Accept-Encoding".parse().unwrap());
    }
    out_headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"")
            .parse()
            .map_err(internal_error)?,
    );
    Ok(resp)
}

/// Import a mission bundle previously produced by [`export_mission`].
///
/// Body is the raw JSON bundle (no multipart wrapper — keep the happy path
/// simple for curl / scripts). Optional query params:
/// - `workspace_id=<uuid>` — override target workspace; otherwise we resolve
///   the bundle's `workspace_name` against the local workspace store.
/// - `keep_automations_active=true` — import automations enabled. Default is
///   disabled so bundles don't immediately start firing on the target.
#[derive(Debug, Deserialize, Default)]
pub struct ImportMissionQuery {
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub keep_automations_active: bool,
}

pub async fn import_mission(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    axum::extract::Query(query): axum::extract::Query<ImportMissionQuery>,
    headers_in: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let bundle = parse_mission_bundle(&headers_in, &body)?;
    if bundle.version != 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported bundle version {} (expected 1)", bundle.version),
        ));
    }

    let target = resolve_import_target_workspace(
        &state,
        query.workspace_id,
        bundle.workspace_name.as_deref(),
    )
    .await?;

    let options = mission_store::MissionImportOptions {
        target_workspace_id: Some(target.workspace_id),
        target_workspace_name: target.workspace_name.clone(),
        keep_automations_active: query.keep_automations_active,
    };
    let original_mission_id = bundle.mission.id;
    let events_imported = bundle.events.len();
    let automations_imported = bundle.automations.len();
    let executions_imported = bundle.executions.len();
    let new_id = control
        .mission_store
        .import_mission_bundle(bundle, options)
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "mission_id": new_id,
        "workspace_id": target.workspace_id,
        "original_mission_id": original_mission_id,
        "imported": {
            "events": events_imported,
            "automations": automations_imported,
            "executions": executions_imported,
        },
        "automations_active": query.keep_automations_active,
    })))
}

/// Resolve the workspace a mission import should land in.
///
/// Explicit `?workspace_id=` wins. Otherwise the bundle's
/// `workspace_name` is matched against the local workspace list. A
/// single match is used; zero matches or multiple matches return
/// `BAD_REQUEST`/`CONFLICT` asking the caller to disambiguate — we
/// never silently pick "the first" when workspace names aren't unique.
/// Resolved import target: the workspace UUID the new mission will be
/// attached to, plus its current display name (used so
/// `mission.workspace_name` agrees with `mission.workspace_id`).
struct ResolvedImportTarget {
    workspace_id: Uuid,
    workspace_name: Option<String>,
}

async fn resolve_import_target_workspace(
    state: &Arc<AppState>,
    explicit_id: Option<Uuid>,
    bundle_workspace_name: Option<&str>,
) -> Result<ResolvedImportTarget, (StatusCode, String)> {
    if let Some(id) = explicit_id {
        // Reject unknown IDs up front — the alternative is persisting a
        // mission pointing at a nonexistent workspace, which later falls
        // through to the default host workspace in resolve_workspace and
        // silently executes in the wrong directory.
        let workspaces = state.workspaces.list().await;
        let Some(ws) = workspaces.iter().find(|w| w.id == id) else {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Workspace '{id}' not found on this instance. Pass a \
                     valid ?workspace_id=<uuid> or omit it to resolve \
                     by workspace_name."
                ),
            ));
        };
        return Ok(ResolvedImportTarget {
            workspace_id: id,
            workspace_name: Some(ws.name.clone()),
        });
    }
    let Some(name) = bundle_workspace_name else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Bundle has no workspace_name and no ?workspace_id= override was provided.".to_string(),
        ));
    };
    let workspaces = state.workspaces.list().await;
    let matches: Vec<_> = workspaces.iter().filter(|w| w.name == name).collect();
    match matches.as_slice() {
        [] => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Workspace '{name}' not found on this instance. Pass \
                 ?workspace_id=<uuid> to pick one explicitly."
            ),
        )),
        [only] => Ok(ResolvedImportTarget {
            workspace_id: only.id,
            workspace_name: Some(only.name.clone()),
        }),
        _ => Err((
            StatusCode::CONFLICT,
            format!(
                "Workspace name '{name}' is ambiguous ({} matches). \
                 Pass ?workspace_id=<uuid> to pick one explicitly.",
                matches.len()
            ),
        )),
    }
}

/// Parse a mission bundle from raw bytes, transparently decompressing
/// `Content-Encoding: gzip` before JSON-decoding. Both the single-shot
/// `/import` route and the chunked `/import-chunks/:id/commit` route
/// share this helper — they both arrive as a `Bytes` blob that may or
/// may not be gzipped.
/// Hard cap on decompressed mission bundle size for the *chunked*
/// import path, where the decoder streams from disk-staged chunks into
/// `serde_json::from_reader` without ever buffering the full payload.
/// 2 GiB bounds a zip-bomb there.
const MISSION_BUNDLE_MAX_DECOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Tighter cap for the *single-shot* `/import` route. That path streams
/// the decompressed body into `from_reader`, but the parser still
/// allocates per-frame state up to this ceiling — and an attacker can
/// drive it from a 128 MiB compressed body. Keep the ceiling small
/// enough that even the worst-case allocation stays bounded; larger
/// bundles must use the chunked route, which stages chunks on disk
/// first.
const MISSION_BUNDLE_SINGLE_SHOT_MAX_DECOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// Stream a mission bundle out of a staged chunk directory without ever
/// holding the concatenated body in memory. Handles gzip transparently —
/// either because the caller set `?gzip=true` or because the first chunk
/// starts with the gzip magic header.
fn parse_mission_bundle_from_chunk_dir(
    dir: &std::path::Path,
    total_chunks: u32,
    gzip_hint: bool,
) -> Result<mission_store::MissionBundle, (StatusCode, String)> {
    use std::fs::File;
    use std::io::{BufReader, Read};

    if total_chunks == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "total_chunks must be > 0".to_string(),
        ));
    }

    // Peek at the first chunk to auto-detect gzip when the caller
    // forgot to pass the hint. A plain `[0x1f, 0x8b]` prefix is
    // unambiguous for gzip and lets us decompress without buffering
    // the whole upload first.
    let first_path = dir.join(format!("chunk_{:06}", 0));
    let mut first = File::open(&first_path).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Chunk 0 missing or unreadable: {e}"),
        )
    })?;
    let mut magic = [0u8; 2];
    let peeked = first.read(&mut magic).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Failed to read chunk 0: {e}"),
        )
    })?;
    let is_gzipped = gzip_hint || (peeked == 2 && magic == [0x1f, 0x8b]);
    // Seek back to start so the reader chain sees the full first chunk.
    use std::io::Seek;
    first
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("seek: {e}")))?;

    // Build a chained reader across every chunk file in index order.
    // `Read::chain` is associative, so we fold left across the remaining
    // chunk indices without allocating intermediate buffers.
    let mut reader: Box<dyn Read> = Box::new(BufReader::new(first));
    for i in 1..total_chunks {
        let path = dir.join(format!("chunk_{i:06}"));
        let f = File::open(&path).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Chunk {i} missing or unreadable: {e}"),
            )
        })?;
        reader = Box::new(reader.chain(BufReader::new(f)));
    }

    let bundle: mission_store::MissionBundle = if is_gzipped {
        let decoder = flate2::read::GzDecoder::new(reader);
        let bounded = decoder.take(MISSION_BUNDLE_MAX_DECOMPRESSED_BYTES);
        serde_json::from_reader(BufReader::new(bounded)).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid mission bundle (after gunzip): {e}"),
            )
        })?
    } else {
        // Cap the plain-JSON path at the same ceiling as the gzip
        // branch. `/import-chunks` accepts many 128 MB chunks, so
        // without a cap an attacker could chain them into an
        // arbitrarily large payload and drive unbounded allocation
        // inside serde_json::from_reader.
        let bounded = reader.take(MISSION_BUNDLE_MAX_DECOMPRESSED_BYTES);
        serde_json::from_reader(BufReader::new(bounded)).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid mission bundle: {e}"),
            )
        })?
    };

    Ok(bundle)
}

/// `std::io::Write` adapter that pushes ~64 KB chunks into an mpsc
/// channel consumed by the HTTP response body. Lets `export_mission`
/// serialize (and optionally gzip) the bundle straight to the wire
/// without ever holding the full payload in RAM.
struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    /// Flush threshold — large enough to amortize channel overhead,
    /// small enough that stream backpressure is responsive.
    const CHUNK_BYTES: usize = 64 * 1024;

    fn new(tx: tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(Self::CHUNK_BYTES * 2),
        }
    }

    /// Drain whatever is in `buf`.
    fn flush_all(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            self.tx
                .blocking_send(Ok(bytes::Bytes::from(chunk)))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))?;
        }
        Ok(())
    }

    /// Surface a late error (serialization or flush) to the consumer
    /// by pushing it as a stream item. Best-effort: if the receiver is
    /// already gone we swallow it.
    fn send_error(&self, err: std::io::Error) {
        let _ = self.tx.blocking_send(Err(err));
    }
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= Self::CHUNK_BYTES {
            self.flush_all()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_all()
    }
}

/// Parse an `Accept-Encoding` header and decide whether the client
/// allows gzip. Treats `identity;q=0` / `*;q=0` semantics correctly —
/// a token of the form `gzip;q=0` explicitly disallows gzip, so simply
/// matching on `starts_with("gzip")` (the previous behavior) returns
/// the wrong answer for proxies that send weighted encodings.
fn accept_encoding_allows_gzip(header_value: &str) -> bool {
    let mut gzip_seen = false;
    let mut gzip_qzero = false;
    let mut star_qzero = false;
    for raw in header_value.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        // Split into `name` and optional `q=<float>` parameter.
        let mut parts = token.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let mut q: f32 = 1.0;
        for param in parts {
            let p = param.trim();
            if let Some(rest) = p.strip_prefix("q=").or_else(|| p.strip_prefix("Q=")) {
                if let Ok(v) = rest.parse::<f32>() {
                    q = v;
                }
            }
        }
        let disallowed = q <= 0.0;
        match name.as_str() {
            "gzip" | "x-gzip" => {
                if disallowed {
                    gzip_qzero = true;
                } else {
                    gzip_seen = true;
                }
            }
            "*" => {
                if disallowed {
                    star_qzero = true;
                } else if !gzip_qzero {
                    gzip_seen = true;
                }
            }
            _ => {}
        }
    }
    // Explicit `gzip;q=0` always wins over `*`. Otherwise gzip is on
    // only when the client named it with a positive q-value — a bare
    // `*` (without q=0) could mean "any", but we stay conservative and
    // only compress when explicitly asked.
    if gzip_qzero {
        return false;
    }
    let _ = star_qzero; // star handling is advisory only here
    gzip_seen
}

fn parse_mission_bundle(
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<mission_store::MissionBundle, (StatusCode, String)> {
    use axum::http::header;
    use flate2::read::GzDecoder;
    use std::io::Read;

    let is_gzipped = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("gzip")))
        .unwrap_or(false)
        // Also sniff the gzip magic header — CLIs uploading gzipped
        // bodies via `curl --data-binary @file.gz` sometimes forget to
        // set Content-Encoding, and hitting them with "invalid JSON"
        // instead of decoding would be a confusing footgun.
        || body.starts_with(&[0x1f, 0x8b]);

    // Stream straight from the body into `from_reader` and cap both
    // the compressed wire size (enforced at the route layer via
    // DefaultBodyLimit) and the decompressed size (via `Read::take`)
    // so a zip-bomb can't allocate more than the single-shot ceiling
    // inside the JSON parser.
    let bundle: mission_store::MissionBundle = if is_gzipped {
        let decoder = GzDecoder::new(body).take(MISSION_BUNDLE_SINGLE_SHOT_MAX_DECOMPRESSED_BYTES);
        serde_json::from_reader(std::io::BufReader::new(decoder)).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid mission bundle (after gunzip): {e}"),
            )
        })?
    } else {
        let reader = body.take(MISSION_BUNDLE_SINGLE_SHOT_MAX_DECOMPRESSED_BYTES);
        serde_json::from_reader(std::io::BufReader::new(reader)).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid mission bundle: {e}"),
            )
        })?
    };

    Ok(bundle)
}

fn sanitize_upload_id(id: &str) -> Option<String> {
    // Keep upload IDs to a conservative alphabet — these become path
    // components under /tmp, so allowing only [A-Za-z0-9_-] avoids path
    // traversal without sacrificing UUIDs or custom labels.
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn import_chunks_dir(upload_id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sandboxed_sh_mission_import_{upload_id}"))
}

/// Staging TTL. Any chunked import dir untouched for longer than this is
/// considered abandoned (client disconnect, crash, never committed) and
/// gets swept on the next `init_mission_import` call. Picked large
/// enough that a slow human-driven upload won't get culled mid-flight.
const MISSION_IMPORT_STAGING_TTL: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// Best-effort cleanup of abandoned chunked-import staging dirs. Runs
/// opportunistically on `init_mission_import`; errors are logged but not
/// propagated because temp directory sweeping must never block an
/// otherwise-valid new upload.
fn sweep_stale_import_staging_dirs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(name_os) = entry.file_name().into_string() else {
            continue;
        };
        if !name_os.starts_with("sandboxed_sh_mission_import_") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        // Prefer mtime over ctime — a chunk upload touches mtime, so a
        // currently-active upload looks "fresh" even if init_ was hours
        // ago. Fall back to created() if mtime isn't available.
        let last_touched = metadata
            .modified()
            .or_else(|_| metadata.created())
            .unwrap_or(now);
        match now.duration_since(last_touched) {
            Ok(age) if age > MISSION_IMPORT_STAGING_TTL => {
                let path = entry.path();
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!(?path, error = %e, "Failed to sweep stale mission-import staging dir");
                } else {
                    tracing::info!(
                        ?path,
                        age_secs = age.as_secs(),
                        "Swept abandoned mission-import staging dir"
                    );
                }
            }
            _ => {}
        }
    }
}

/// Initialize a chunked mission import.
///
/// Returns `{"upload_id": "..."}` — the caller then PUTs chunks in order
/// to `/api/control/missions/import-chunks/:upload_id/:index`, and POSTs
/// `/commit` to finalize. This exists for bundles that exceed
/// Cloudflare's 100 MB per-request cap even after gzip (very long
/// missions with binary-heavy payloads that don't compress well).
///
/// For bundles under ~90 MB gzipped, prefer the single-shot `/import`
/// route — it's simpler and doesn't leave temp files on disk.
pub async fn init_mission_import(
    State(_state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Opportunistic cleanup of prior abandoned uploads so /tmp can't
    // grow without bound. Runs on a blocking thread since it hits the
    // filesystem synchronously.
    tokio::task::spawn_blocking(sweep_stale_import_staging_dirs);

    let upload_id = Uuid::new_v4().simple().to_string();
    let dir = import_chunks_dir(&upload_id);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create chunk staging dir: {e}"),
        )
    })?;
    Ok(Json(serde_json::json!({
        "upload_id": upload_id,
        "recommended_chunk_bytes": 80 * 1024 * 1024,
    })))
}

/// Upload one chunk of a mission import.
///
/// Path: `/api/control/missions/import-chunks/:upload_id/:index`.
/// Chunks are written to `chunk_<index:06>` under the upload's staging
/// directory. Index order determines assembly order — the commit step
/// reads `chunk_000000`, `chunk_000001`, ... in sequence.
pub async fn upload_mission_import_chunk(
    State(_state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path((upload_id, index)): Path<(String, u32)>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let safe_id = sanitize_upload_id(&upload_id)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid upload_id".to_string()))?;
    let dir = import_chunks_dir(&safe_id);
    if !dir.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            "Unknown upload_id. Call /import-chunks first to initialize.".to_string(),
        ));
    }
    let chunk_path = dir.join(format!("chunk_{index:06}"));
    tokio::fs::write(&chunk_path, &body).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write chunk {index}: {e}"),
        )
    })?;
    Ok(Json(serde_json::json!({
        "upload_id": safe_id,
        "chunk_index": index,
        "chunk_bytes": body.len(),
    })))
}

#[derive(Debug, Deserialize, Default)]
pub struct CommitMissionImportQuery {
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub keep_automations_active: bool,
    /// Total number of chunks the client uploaded. Must match the files
    /// present in the staging dir — any gap aborts the commit.
    pub total_chunks: u32,
    /// Hint: the bundle is gzipped (set `?gzip=true` when sending
    /// compressed chunks; otherwise server sniffs the magic header).
    #[serde(default)]
    pub gzip: bool,
}

/// Assemble uploaded chunks, optionally decompress, and run the regular
/// import.
///
/// Cleans up the staging directory on success or failure — a caller that
/// wants to retry a failed commit must re-upload all chunks under a
/// fresh `upload_id`.
pub async fn commit_mission_import(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(upload_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CommitMissionImportQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let safe_id = sanitize_upload_id(&upload_id)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid upload_id".to_string()))?;
    let dir = import_chunks_dir(&safe_id);
    if !dir.exists() {
        return Err((StatusCode::NOT_FOUND, "Unknown upload_id.".to_string()));
    }

    // Parse directly from the staged chunk files instead of concatenating
    // them into one giant `Vec<u8>` in memory. For multi-hundred-MB or GB
    // uploads that's the difference between "handled" and "OOM'd the
    // daemon". The parser streams via `serde_json::from_reader` on a
    // chained `File` iterator (optionally gzip-decoded in flight).
    let total_chunks = query.total_chunks;
    let gzip_hint = query.gzip;
    let dir_for_parse = dir.clone();
    let bundle_result = tokio::task::spawn_blocking(move || {
        parse_mission_bundle_from_chunk_dir(&dir_for_parse, total_chunks, gzip_hint)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Join error while parsing import: {e}"),
        )
    })?;

    // Chunks on disk are no longer needed — wipe staging regardless of
    // parse outcome so /tmp doesn't hold the raw upload open.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    let bundle = bundle_result?;
    if bundle.version != 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Unsupported bundle version {} (expected 1)", bundle.version),
        ));
    }

    let target = resolve_import_target_workspace(
        &state,
        query.workspace_id,
        bundle.workspace_name.as_deref(),
    )
    .await?;

    let control = control_for_user(&state, &user).await;
    let options = mission_store::MissionImportOptions {
        target_workspace_id: Some(target.workspace_id),
        target_workspace_name: target.workspace_name.clone(),
        keep_automations_active: query.keep_automations_active,
    };
    let original_mission_id = bundle.mission.id;
    let events_imported = bundle.events.len();
    let automations_imported = bundle.automations.len();
    let executions_imported = bundle.executions.len();
    let new_id = control
        .mission_store
        .import_mission_bundle(bundle, options)
        .await
        .map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "mission_id": new_id,
        "workspace_id": target.workspace_id,
        "original_mission_id": original_mission_id,
        "imported": {
            "events": events_imported,
            "automations": automations_imported,
            "executions": executions_imported,
        },
        "automations_active": query.keep_automations_active,
    })))
}

/// Cancel an in-progress chunked import and remove its staging dir.
pub async fn cancel_mission_import(
    State(_state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Path(upload_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let safe_id = sanitize_upload_id(&upload_id)
        .ok_or((StatusCode::BAD_REQUEST, "Invalid upload_id".to_string()))?;
    let dir = import_chunks_dir(&safe_id);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir)
            .await
            .map_err(internal_error)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Webhook receiver endpoint for triggering automations.
/// Accepts POST requests with JSON body and validates webhook secret if configured.
pub async fn webhook_receiver(
    State(state): State<Arc<AppState>>,
    Path((mission_id, webhook_id)): Path<(Uuid, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, String)> {
    use super::automation_variables::{
        apply_webhook_mappings, substitute_variables, SubstitutionContext,
    };
    use super::mission_store::{AutomationExecution, CommandSource, ExecutionStatus, TriggerType};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid JSON payload: {}", e),
        )
    })?;

    // Search across all user sessions for the webhook automation.
    // Automations are user-scoped, so we must check every session's mission store.
    let sessions = state.control.all_sessions().await;
    let mut found: Option<(mission_store::Automation, ControlState)> = None;
    for session in &sessions {
        match session
            .mission_store
            .get_automation_by_webhook_id(&webhook_id)
            .await
        {
            Ok(Some(automation)) => {
                found = Some((automation, session.clone()));
                break;
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("Error searching webhook {} in session: {}", webhook_id, e);
                continue;
            }
        }
    }

    let (automation, control) = found.ok_or((
        StatusCode::NOT_FOUND,
        format!("Webhook {} not found", webhook_id),
    ))?;

    // Verify mission_id matches
    if automation.mission_id != mission_id {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Webhook {} does not belong to mission {}",
                webhook_id, mission_id
            ),
        ));
    }

    // Check if automation is active
    if !automation.active {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Automation {} is not active", automation.id),
        ));
    }

    // Extract webhook config
    let webhook_config = match &automation.trigger {
        TriggerType::Webhook { config } => config,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Automation is not configured for webhook trigger".to_string(),
            ));
        }
    };

    // Validate webhook secret if configured (HMAC-SHA256)
    if let Some(ref secret) = webhook_config.secret {
        // Check for signature in headers (support both GitHub and generic formats)
        let signature_header = headers
            .get("x-hub-signature-256")
            .or_else(|| headers.get("x-webhook-signature"))
            .and_then(|v| v.to_str().ok());

        if let Some(signature) = signature_header {
            let signature = signature.trim();
            let signature = signature.strip_prefix("sha256=").unwrap_or(signature);
            let signature_bytes = hex::decode(signature).map_err(|_| {
                (
                    StatusCode::UNAUTHORIZED,
                    "Invalid webhook signature".to_string(),
                )
            })?;

            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Invalid webhook secret".to_string(),
                )
            })?;
            mac.update(&body);

            if mac.verify_slice(&signature_bytes).is_err() {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Invalid webhook signature".to_string(),
                ));
            }
        } else {
            return Err((
                StatusCode::UNAUTHORIZED,
                "Missing webhook signature".to_string(),
            ));
        }
    }

    // Get mission
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("Mission {} not found", mission_id),
        ))?;

    let consecutive_failures =
        consecutive_failure_count_for_automation(&control.mission_store, &automation).await;

    if stop_policy_matches_status(
        &automation.stop_policy,
        mission.status,
        consecutive_failures,
    )
    .await
    {
        let mut updated = automation.clone();
        updated.active = false;
        if let Err(e) = control.mission_store.update_automation(updated).await {
            tracing::warn!(
                "Failed to disable webhook automation {} after stop policy match: {}",
                automation.id,
                e
            );
        }
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Automation {} is stopped by policy {:?} for mission status {:?}",
                automation.id, automation.stop_policy, mission.status
            ),
        ));
    }

    // Get workspace for reading local files
    let workspace = state.workspaces.get(mission.workspace_id).await;

    // Fetch the command content based on the command source
    let command_content = match &automation.command_source {
        CommandSource::Library { name } => {
            if let Some(lib) = state.library.read().await.as_ref() {
                match lib.get_command(name.as_str()).await {
                    Ok(command) => automation_library_command_body(&command.content),
                    Err(e) => {
                        return Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to fetch command '{}': {}", name, e),
                        ));
                    }
                }
            } else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Library not initialized".to_string(),
                ));
            }
        }
        CommandSource::LocalFile { path } => {
            // Read file from mission workspace
            let file_path = if let Some(ws) = workspace.as_ref() {
                ws.path.join(path)
            } else {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Workspace {} not found", mission.workspace_id),
                ));
            };

            match tokio::fs::read_to_string(&file_path).await {
                Ok(content) => content,
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to read file '{}': {}", file_path.display(), e),
                    ));
                }
            }
        }
        CommandSource::Inline { content } => content.clone(),
    };

    // Apply webhook variable mappings
    let webhook_vars = apply_webhook_mappings(&payload, &webhook_config.variable_mappings);

    // Extract direct "variables" from payload (allows callers to pass {"variables": {"key": "value"}})
    let direct_vars: HashMap<String, String> = payload
        .get("variables")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    // Build substitution context for variable replacement
    let mut context = SubstitutionContext::new(mission.id);
    if let Some(ref title) = mission.title {
        context = context.with_mission_name(title.clone());
    }
    if let Some(ws) = workspace.as_ref() {
        context = context.with_working_directory(ws.path.to_string_lossy().to_string());
    }
    context = context.with_webhook_payload(payload.clone());

    // Merge variables: automation defaults < webhook mappings < direct variables (highest priority)
    let mut merged_vars = automation.variables.clone();
    merged_vars.extend(webhook_vars.clone());
    merged_vars.extend(direct_vars);
    context = context.with_custom_variables(merged_vars.clone());

    // Apply variable substitution
    let substituted_content = substitute_variables(&command_content, &context);

    // Create execution record
    let execution_id = Uuid::new_v4();
    let execution = AutomationExecution {
        id: execution_id,
        automation_id: automation.id,
        mission_id: mission.id,
        triggered_at: mission_store::now_string(),
        trigger_source: "webhook".to_string(),
        status: ExecutionStatus::Pending,
        webhook_payload: Some(payload),
        variables_used: merged_vars,
        completed_at: None,
        error: None,
        retry_count: 0,
    };

    let mut execution = match control
        .mission_store
        .create_automation_execution(execution)
        .await
    {
        Ok(exec) => exec,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create execution record: {}", e),
            ));
        }
    };

    tracing::info!(
        "Webhook {} triggered automation {} (execution {}) for mission {}",
        webhook_id,
        automation.id,
        execution_id,
        mission.id
    );

    // Update execution status to Running
    execution.status = ExecutionStatus::Running;
    if let Err(e) = control
        .mission_store
        .update_automation_execution(execution.clone())
        .await
    {
        tracing::warn!(
            "Failed to update execution status to running for {}: {}",
            execution_id,
            e
        );
    }

    // Send the message to the mission
    let message_id = Uuid::new_v4();
    let (respond_tx, _respond_rx) = tokio::sync::oneshot::channel();

    let cmd_tx = control.cmd_tx.clone();
    let mission_store = control.mission_store.clone();
    drop(control); // Release the lock before sending

    let send_result = cmd_tx
        .send(ControlCommand::UserMessage {
            id: message_id,
            content: substituted_content,
            agent: None,
            target_mission_id: Some(mission.id),
            respond: respond_tx,
        })
        .await;

    match send_result {
        Ok(_) => {
            // Message queued successfully – keep execution in Running
            // status. The actual success/failure will be determined
            // when the agent finishes processing and
            // complete_running_executions_for_mission is called.
            if let Err(e) = mission_store
                .update_automation_last_triggered(automation.id)
                .await
            {
                tracing::warn!(
                    "Failed to update automation last triggered time for {}: {}",
                    automation.id,
                    e
                );
            }

            Ok(StatusCode::OK)
        }
        Err(e) => {
            // Failed to even send the message – mark as Failed immediately
            execution.status = ExecutionStatus::Failed;
            execution.completed_at = Some(mission_store::now_string());
            execution.error = Some(format!("Failed to send message: {}", e));

            if let Err(e) = mission_store.update_automation_execution(execution).await {
                tracing::warn!(
                    "Failed to update execution status to failed for {}: {}",
                    execution_id,
                    e
                );
            }

            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to trigger automation: {}", e),
            ))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Assistant missions
// ─────────────────────────────────────────────────────────────────────────────

/// List all missions with MissionMode::Assistant.
pub async fn list_assistant_missions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<super::mission_store::Mission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let missions = control
        .mission_store
        .list_assistant_missions()
        .await
        .map_err(internal_error)?;
    Ok(Json(missions))
}

/// Set mission mode (task or assistant).
pub async fn set_mission_mode(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMissionModeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use super::mission_store::MissionMode;

    let mode = match req.mode.as_str() {
        "task" => MissionMode::Task,
        "assistant" => MissionMode::Assistant,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Invalid mission mode: {other}. Must be 'task' or 'assistant'."),
            ));
        }
    };

    let control = control_for_user(&state, &user).await;
    control
        .mission_store
        .update_mission_mode(id, mode)
        .await
        .map_err(internal_error)?;

    Ok(ok_json())
}

#[derive(Deserialize)]
pub struct SetMissionModeRequest {
    pub mode: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Telegram Channel endpoints
// ─────────────────────────────────────────────────────────────────────────────

/// List Telegram channels for a mission.
pub async fn list_telegram_channels(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
) -> Result<Json<Vec<super::mission_store::TelegramChannel>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let channels = control
        .mission_store
        .list_telegram_channels(mission_id)
        .await
        .map_err(internal_error)?;
    Ok(Json(channels))
}

/// Create a Telegram channel for a mission.
pub async fn create_telegram_channel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(mission_id): Path<Uuid>,
    Json(req): Json<CreateTelegramChannelRequest>,
) -> Result<Json<super::mission_store::TelegramChannel>, (StatusCode, String)> {
    use super::mission_store::{now_string, MissionMode, TelegramChannel};

    let control = control_for_user(&state, &user).await;

    // Verify mission exists
    let mission = control
        .mission_store
        .get_mission(mission_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Mission {} not found", mission_id),
            )
        })?;

    // Auto-set mission to Assistant mode when adding a Telegram channel
    if mission.mission_mode != MissionMode::Assistant {
        if let Err(e) = control
            .mission_store
            .update_mission_mode(mission_id, MissionMode::Assistant)
            .await
        {
            tracing::warn!(
                "Failed to set assistant mode on mission {}: {}",
                mission_id,
                e
            );
        } else {
            tracing::info!(
                "Auto-set mission {} to Assistant mode (Telegram channel attached)",
                mission_id
            );
        }
    }

    // Reject duplicate bot tokens to avoid webhook conflicts
    let all_channels = control
        .mission_store
        .list_all_telegram_channels()
        .await
        .map_err(internal_error)?;
    if all_channels.iter().any(|c| c.bot_token == req.bot_token) {
        return Err((
            StatusCode::CONFLICT,
            "A channel with this bot token already exists".to_string(),
        ));
    }

    let now = now_string();
    let webhook_secret = Uuid::new_v4().to_string().replace('-', "");
    let channel = TelegramChannel {
        id: Uuid::new_v4(),
        mission_id,
        bot_token: req.bot_token,
        bot_username: req.bot_username,
        allowed_chat_ids: req.allowed_chat_ids.unwrap_or_default(),
        trigger_mode: req.trigger_mode.unwrap_or_default(),
        active: true,
        webhook_secret: Some(webhook_secret),
        instructions: req.instructions,
        auto_create_missions: false,
        default_backend: None,
        default_model_override: None,
        default_model_effort: None,
        default_workspace_id: None,
        default_config_profile: None,
        default_agent: None,
        created_at: now.clone(),
        updated_at: now,
    };

    let created = control
        .mission_store
        .create_telegram_channel(channel)
        .await
        .map_err(internal_error)?;

    // Register the webhook — roll back the channel if this fails
    let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
    if let Err(e) = state
        .telegram_bridge
        .start_channel(
            created.clone(),
            control.cmd_tx.clone(),
            control.events_tx.clone(),
            control.mission_store.clone(),
            &public_url,
        )
        .await
    {
        let _ = control
            .mission_store
            .delete_telegram_channel(created.id)
            .await;
        return Err(internal_error(e));
    }

    tracing::info!(
        "Created Telegram channel {} for mission {}",
        created.id,
        mission_id
    );

    Ok(Json(created))
}

#[derive(Deserialize)]
pub struct CreateTelegramChannelRequest {
    pub bot_token: String,
    #[serde(default)]
    pub bot_username: Option<String>,
    #[serde(default)]
    pub allowed_chat_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub trigger_mode: Option<super::mission_store::TelegramTriggerMode>,
    /// System instructions for the assistant (e.g. "Don't use markdown formatting")
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Delete a Telegram channel.
pub async fn delete_telegram_channel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    // Fetch channel first so we can clean up the placeholder mission for auto-create bots
    let channel = control
        .mission_store
        .get_telegram_channel(channel_id)
        .await
        .map_err(internal_error)?;

    // Delete from store first (verifies ownership), then stop the poller
    let deleted = control
        .mission_store
        .delete_telegram_channel(channel_id)
        .await
        .map_err(internal_error)?;

    if deleted {
        state.telegram_bridge.stop_channel(channel_id).await;

        // Clean up the placeholder mission for auto-create bots
        if let Some(ch) = channel {
            if ch.auto_create_missions {
                let _ = control.mission_store.delete_mission(ch.mission_id).await;
            }
        }

        tracing::info!("Deleted Telegram channel {}", channel_id);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("Telegram channel {} not found", channel_id),
        ))
    }
}

/// Toggle a Telegram channel's active state.
pub async fn toggle_telegram_channel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Json(req): Json<ToggleTelegramChannelRequest>,
) -> Result<Json<super::mission_store::TelegramChannel>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let mut channel = control
        .mission_store
        .get_telegram_channel(channel_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Telegram channel {} not found", channel_id),
            )
        })?;

    let previous_active = channel.active;
    channel.active = req.active;
    channel.updated_at = super::mission_store::now_string();

    control
        .mission_store
        .update_telegram_channel(channel.clone())
        .await
        .map_err(internal_error)?;

    if channel.active {
        let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
        if let Err(e) = state
            .telegram_bridge
            .start_channel(
                channel.clone(),
                control.cmd_tx.clone(),
                control.events_tx.clone(),
                control.mission_store.clone(),
                &public_url,
            )
            .await
        {
            // Roll back active state
            channel.active = previous_active;
            channel.updated_at = super::mission_store::now_string();
            let _ = control.mission_store.update_telegram_channel(channel).await;
            return Err(internal_error(e));
        }
    } else {
        state.telegram_bridge.stop_channel(channel_id).await;
    }

    Ok(Json(channel))
}

#[derive(Deserialize)]
pub struct ToggleTelegramChannelRequest {
    pub active: bool,
}

/// Update a Telegram channel's settings (PATCH).
pub async fn update_telegram_channel(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Json(req): Json<UpdateTelegramChannelRequest>,
) -> Result<Json<super::mission_store::TelegramChannel>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    let original_channel = control
        .mission_store
        .get_telegram_channel(channel_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Telegram channel {} not found", channel_id),
            )
        })?;

    let mut channel = original_channel.clone();

    // Apply partial updates
    if let Some(active) = req.active {
        channel.active = active;
    }
    if let Some(trigger_mode) = req.trigger_mode {
        channel.trigger_mode = trigger_mode;
    }
    if let Some(allowed_chat_ids) = req.allowed_chat_ids {
        channel.allowed_chat_ids = allowed_chat_ids;
    }
    if let Some(instructions) = req.instructions {
        channel.instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions)
        };
    }
    if let Some(backend) = req.default_backend {
        channel.default_backend = if backend.is_empty() {
            None
        } else {
            Some(backend)
        };
    }
    if let Some(model) = req.default_model_override {
        channel.default_model_override = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(effort) = req.default_model_effort {
        channel.default_model_effort = if effort.is_empty() {
            None
        } else {
            Some(effort)
        };
    }
    if let Some(ws_id) = req.default_workspace_id {
        channel.default_workspace_id = if ws_id.is_empty() {
            None
        } else {
            uuid::Uuid::parse_str(&ws_id).ok()
        };
    }
    if let Some(profile) = req.default_config_profile {
        channel.default_config_profile = if profile.is_empty() {
            None
        } else {
            Some(profile)
        };
    }
    if let Some(agent) = req.default_agent {
        channel.default_agent = if agent.is_empty() { None } else { Some(agent) };
    }
    channel.updated_at = super::mission_store::now_string();

    control
        .mission_store
        .update_telegram_channel(channel.clone())
        .await
        .map_err(internal_error)?;

    // Re-register or stop webhook based on active state
    if channel.active {
        let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
            .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
        if let Err(e) = state
            .telegram_bridge
            .start_channel(
                channel.clone(),
                control.cmd_tx.clone(),
                control.events_tx.clone(),
                control.mission_store.clone(),
                &public_url,
            )
            .await
        {
            // Roll back to original channel state
            let _ = control
                .mission_store
                .update_telegram_channel(original_channel)
                .await;
            return Err(internal_error(e));
        }
    } else {
        state.telegram_bridge.stop_channel(channel_id).await;
    }

    Ok(Json(channel))
}

#[derive(Deserialize)]
pub struct UpdateTelegramChannelRequest {
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub trigger_mode: Option<super::mission_store::TelegramTriggerMode>,
    #[serde(default)]
    pub allowed_chat_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub default_backend: Option<String>,
    #[serde(default)]
    pub default_model_override: Option<String>,
    #[serde(default)]
    pub default_model_effort: Option<String>,
    #[serde(default)]
    pub default_workspace_id: Option<String>,
    #[serde(default)]
    pub default_config_profile: Option<String>,
    #[serde(default)]
    pub default_agent: Option<String>,
}

// === Standalone Telegram Bot endpoints (auto-create missions per chat) ===

/// Create a standalone Telegram bot configuration (auto-creates missions per chat).
pub async fn create_telegram_bot(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateTelegramBotRequest>,
) -> Result<Json<super::mission_store::TelegramChannel>, (StatusCode, String)> {
    use super::mission_store::{now_string, MissionMode, TelegramChannel};

    let control = control_for_user(&state, &user).await;

    // Reject duplicate bot tokens to avoid webhook conflicts
    let all_channels = control
        .mission_store
        .list_all_telegram_channels()
        .await
        .map_err(internal_error)?;
    if all_channels.iter().any(|c| c.bot_token == req.bot_token) {
        return Err((
            StatusCode::CONFLICT,
            "A bot with this token already exists".to_string(),
        ));
    }

    // Create a placeholder mission so the FK constraint is satisfied.
    // When auto_create_missions is true, individual chat missions are auto-created;
    // this placeholder is only used to anchor the channel row.
    let placeholder_mission = control
        .mission_store
        .create_mission(
            Some("Telegram Bot (auto-create)"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .map_err(internal_error)?;
    // Set placeholder to assistant mode
    let _ = control
        .mission_store
        .update_mission_mode(placeholder_mission.id, MissionMode::Assistant)
        .await;

    let now = now_string();
    let webhook_secret = Uuid::new_v4().to_string().replace('-', "");
    let channel = TelegramChannel {
        id: Uuid::new_v4(),
        mission_id: placeholder_mission.id,
        bot_token: req.bot_token,
        bot_username: req.bot_username,
        allowed_chat_ids: req.allowed_chat_ids.unwrap_or_default(),
        trigger_mode: req.trigger_mode.unwrap_or_default(),
        active: true,
        webhook_secret: Some(webhook_secret),
        instructions: req.instructions,
        auto_create_missions: true,
        default_backend: req.default_backend,
        default_model_override: req.default_model_override,
        default_model_effort: req.default_model_effort,
        default_workspace_id: req.default_workspace_id,
        default_config_profile: req.default_config_profile,
        default_agent: req.default_agent,
        created_at: now.clone(),
        updated_at: now,
    };

    let created = match control.mission_store.create_telegram_channel(channel).await {
        Ok(c) => c,
        Err(e) => {
            // Clean up placeholder mission
            let _ = control
                .mission_store
                .delete_mission(placeholder_mission.id)
                .await;
            return Err(internal_error(e));
        }
    };

    // Register the webhook — roll back channel + placeholder on failure
    let public_url = std::env::var("SANDBOXED_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://{}:{}", state.config.host, state.config.port));
    if let Err(e) = state
        .telegram_bridge
        .start_channel(
            created.clone(),
            control.cmd_tx.clone(),
            control.events_tx.clone(),
            control.mission_store.clone(),
            &public_url,
        )
        .await
    {
        let _ = control
            .mission_store
            .delete_telegram_channel(created.id)
            .await;
        let _ = control
            .mission_store
            .delete_mission(placeholder_mission.id)
            .await;
        return Err(internal_error(e));
    }

    tracing::info!("Created Telegram bot {} (auto-create missions)", created.id);

    Ok(Json(created))
}

#[derive(Deserialize)]
pub struct CreateTelegramBotRequest {
    pub bot_token: String,
    #[serde(default)]
    pub bot_username: Option<String>,
    #[serde(default)]
    pub allowed_chat_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub trigger_mode: Option<super::mission_store::TelegramTriggerMode>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub default_backend: Option<String>,
    #[serde(default)]
    pub default_model_override: Option<String>,
    #[serde(default)]
    pub default_model_effort: Option<String>,
    #[serde(default)]
    pub default_workspace_id: Option<Uuid>,
    #[serde(default)]
    pub default_config_profile: Option<String>,
    #[serde(default)]
    pub default_agent: Option<String>,
}

/// List all Telegram bot configurations.
pub async fn list_telegram_bots(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Vec<super::mission_store::TelegramChannel>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let channels = control
        .mission_store
        .list_all_telegram_channels()
        .await
        .map_err(internal_error)?;
    Ok(Json(channels))
}

/// List chat-to-mission mappings for a Telegram bot.
pub async fn list_bot_chats(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
) -> Result<Json<Vec<super::mission_store::TelegramChatMission>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let mappings = control
        .mission_store
        .list_telegram_chat_missions(channel_id)
        .await
        .map_err(internal_error)?;
    Ok(Json(mappings))
}

#[derive(Debug, Deserialize)]
pub struct TelegramBotListQuery {
    #[serde(default = "default_telegram_limit")]
    pub limit: usize,
    #[serde(default)]
    pub chat_id: Option<i64>,
}

fn default_telegram_limit() -> usize {
    20
}

pub async fn list_bot_scheduled_messages(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramScheduledMessage>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let messages = control
        .mission_store
        .list_telegram_scheduled_messages(channel_id, query.chat_id, query.limit.clamp(1, 100))
        .await
        .map_err(internal_error)?;
    Ok(Json(messages))
}

pub async fn list_bot_action_executions(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramActionExecution>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let executions = control
        .mission_store
        .list_telegram_action_executions(channel_id, query.chat_id, query.limit.clamp(1, 100))
        .await
        .map_err(internal_error)?;
    Ok(Json(executions))
}

pub async fn list_bot_conversations(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramConversation>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let conversations = control
        .mission_store
        .list_telegram_conversations(channel_id, query.limit.clamp(1, 100))
        .await
        .map_err(internal_error)?;
    Ok(Json(conversations))
}

pub async fn list_bot_workflows(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramWorkflow>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let workflows = control
        .mission_store
        .list_telegram_workflows(channel_id, query.limit.clamp(1, 100))
        .await
        .map_err(internal_error)?;
    Ok(Json(workflows))
}

pub async fn list_telegram_conversation_messages(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramConversationMessage>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let messages = control
        .mission_store
        .list_telegram_conversation_messages(conversation_id, query.limit.clamp(1, 200))
        .await
        .map_err(internal_error)?;
    Ok(Json(messages))
}

pub async fn list_telegram_workflow_events(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(workflow_id): Path<Uuid>,
    Query(query): Query<TelegramBotListQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramWorkflowEvent>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let events = control
        .mission_store
        .list_telegram_workflow_events(workflow_id, query.limit.clamp(1, 200))
        .await
        .map_err(internal_error)?;
    Ok(Json(events))
}

#[derive(Debug, Deserialize)]
pub struct TelegramMemoryQuery {
    #[serde(default = "default_telegram_limit")]
    pub limit: usize,
    #[serde(default)]
    pub chat_id: Option<i64>,
    #[serde(default)]
    pub subject_user_id: Option<i64>,
    #[serde(default)]
    pub q: Option<String>,
}

pub async fn list_bot_structured_memory(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramMemoryQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramStructuredMemoryEntry>>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let limit = query.limit.clamp(1, 100);
    let entries = if let Some(q) = query.q.as_deref().filter(|q| !q.trim().is_empty()) {
        let mut entries = control
            .mission_store
            .search_telegram_structured_memory_hybrid(
                channel_id,
                query.chat_id,
                query.subject_user_id,
                q,
                limit,
            )
            .await
            .map_err(internal_error)?
            .into_iter()
            .map(|hit| hit.entry)
            .collect::<Vec<_>>();
        if let Some(subject_user_id) = query.subject_user_id {
            entries.retain(|entry| entry.subject_user_id == Some(subject_user_id));
        }
        entries
    } else {
        control
            .mission_store
            .list_telegram_structured_memory(
                channel_id,
                query.chat_id,
                query.subject_user_id,
                limit,
            )
            .await
            .map_err(internal_error)?
    };
    Ok(Json(entries))
}

pub async fn search_bot_structured_memory(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(channel_id): Path<Uuid>,
    Query(query): Query<TelegramMemoryQuery>,
) -> Result<Json<Vec<super::mission_store::TelegramStructuredMemorySearchHit>>, (StatusCode, String)>
{
    let control = control_for_user(&state, &user).await;
    let Some(q) = query.q.as_deref().filter(|q| !q.trim().is_empty()) else {
        return Ok(Json(Vec::new()));
    };
    let mut hits = control
        .mission_store
        .search_telegram_structured_memory_hybrid(
            channel_id,
            query.chat_id,
            query.subject_user_id,
            q,
            query.limit.clamp(1, 100),
        )
        .await
        .map_err(internal_error)?;
    if let Some(subject_user_id) = query.subject_user_id {
        hits.retain(|hit| hit.entry.subject_user_id == Some(subject_user_id));
    }
    Ok(Json(hits))
}

/// Send a message to a Telegram chat via the bot and optionally dispatch it to
/// the associated mission. Used by agents (e.g. Paloma) to proactively message users.
#[derive(Debug, Deserialize)]
pub struct SendTelegramMessageRequest {
    /// Telegram chat ID to send to
    pub chat_id: i64,
    /// Message text
    pub text: String,
    /// Bot channel ID (if omitted, uses the first active bot)
    #[serde(default)]
    pub channel_id: Option<Uuid>,
    /// Also dispatch as a user message to the chat's associated mission
    #[serde(default)]
    pub dispatch_to_mission: bool,
}

#[derive(Debug, Deserialize)]
pub struct TelegramActionRequest {
    pub mission_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub delay_seconds: Option<u64>,
    #[serde(default)]
    pub target: Option<super::telegram::TelegramActionTarget>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramWorkflowRequest {
    pub mission_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub target: Option<super::telegram::TelegramActionTarget>,
}

pub async fn execute_telegram_action_api(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<TelegramActionRequest>,
) -> Result<Json<super::telegram::TelegramActionExecutionResult>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let result = super::telegram::execute_native_telegram_action(
        &state.telegram_bridge,
        &control.mission_store,
        req.mission_id,
        req.target
            .unwrap_or(super::telegram::TelegramActionTarget::Current),
        &req.text,
        req.delay_seconds.unwrap_or(0),
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(result))
}

pub async fn execute_telegram_workflow_request_api(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<TelegramWorkflowRequest>,
) -> Result<Json<super::telegram::TelegramWorkflowRequestResult>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;
    let result = super::telegram::execute_native_telegram_request_workflow(
        &state.telegram_bridge,
        &control.mission_store,
        req.mission_id,
        req.target
            .unwrap_or(super::telegram::TelegramActionTarget::Current),
        &req.text,
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    Ok(Json(result))
}

fn internal_telegram_action_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-sandboxed-mission-token")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .filter(|value| !value.trim().is_empty())
        })
}

async fn mission_store_for_telegram_mission(
    state: &Arc<AppState>,
    mission_id: Uuid,
) -> Option<Arc<dyn MissionStore>> {
    let sessions = state.control.all_sessions().await;
    for session in sessions {
        let has_chat_mapping = session
            .mission_store
            .get_telegram_chat_mission_by_mission_id(mission_id)
            .await
            .ok()
            .flatten()
            .is_some();
        let has_attached_channel = session
            .mission_store
            .list_telegram_channels(mission_id)
            .await
            .map(|channels| !channels.is_empty())
            .unwrap_or(false);
        if has_chat_mapping || has_attached_channel {
            return Some(Arc::clone(&session.mission_store));
        }
    }
    None
}

pub async fn execute_telegram_action_internal_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TelegramActionRequest>,
) -> Result<Json<super::telegram::TelegramActionExecutionResult>, (StatusCode, String)> {
    let token = internal_telegram_action_token(&headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "Missing mission token".to_string(),
        )
    })?;
    if !super::telegram::verify_internal_telegram_action_token(req.mission_id, token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid mission token".to_string(),
        ));
    }

    let mission_store = mission_store_for_telegram_mission(&state, req.mission_id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!(
                    "Mission {} is not linked to an active Telegram conversation",
                    req.mission_id
                ),
            )
        })?;

    let result = super::telegram::execute_native_telegram_action(
        &state.telegram_bridge,
        &mission_store,
        req.mission_id,
        req.target
            .unwrap_or(super::telegram::TelegramActionTarget::Current),
        &req.text,
        req.delay_seconds.unwrap_or(0),
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;

    Ok(Json(result))
}

pub async fn execute_telegram_workflow_request_internal_api(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TelegramWorkflowRequest>,
) -> Result<Json<super::telegram::TelegramWorkflowRequestResult>, (StatusCode, String)> {
    let token = internal_telegram_action_token(&headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "Missing mission token".to_string(),
        )
    })?;
    if !super::telegram::verify_internal_telegram_action_token(req.mission_id, token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid mission token".to_string(),
        ));
    }

    let mission_store = mission_store_for_telegram_mission(&state, req.mission_id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!(
                    "Mission {} is not linked to an active Telegram conversation",
                    req.mission_id
                ),
            )
        })?;

    let result = super::telegram::execute_native_telegram_request_workflow(
        &state.telegram_bridge,
        &mission_store,
        req.mission_id,
        req.target
            .unwrap_or(super::telegram::TelegramActionTarget::Current),
        &req.text,
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, error))?;

    Ok(Json(result))
}

pub async fn send_telegram_message_api(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<SendTelegramMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let control = control_for_user(&state, &user).await;

    // Resolve which bot to use
    let channel = if let Some(channel_id) = req.channel_id {
        control
            .mission_store
            .get_telegram_channel(channel_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Telegram channel {} not found", channel_id),
                )
            })?
    } else {
        // Use first active bot
        let channels = control
            .mission_store
            .list_all_telegram_channels()
            .await
            .map_err(internal_error)?;
        channels.into_iter().find(|c| c.active).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "No active Telegram bot found".to_string(),
            )
        })?
    };

    // Send the message via Telegram Bot API with HTML rendering and chunking
    let base_url = format!("https://api.telegram.org/bot{}", channel.bot_token);
    let http = reqwest::Client::new();
    let msg_id =
        super::telegram::send_telegram_text(&http, &base_url, req.chat_id, &req.text, None)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    let resp_body = serde_json::json!({"ok": true, "message_id": msg_id});

    // Optionally dispatch to the associated mission
    if req.dispatch_to_mission {
        if let Ok(Some(mapping)) = control
            .mission_store
            .get_telegram_chat_mission(channel.id, req.chat_id)
            .await
        {
            let msg_id = Uuid::new_v4();
            let (tx, _rx) = tokio::sync::oneshot::channel();
            let content = format!(
                "[Telegram from @{} in chat {}] {}",
                channel.bot_username.as_deref().unwrap_or("bot"),
                req.chat_id,
                req.text
            );
            let _ = control
                .cmd_tx
                .send(ControlCommand::UserMessage {
                    id: msg_id,
                    content,
                    agent: None,
                    target_mission_id: Some(mapping.mission_id),
                    respond: tx,
                })
                .await;
        }
    }

    Ok(Json(resp_body))
}

/// Telegram webhook receiver (unauthenticated — verified via secret token header).
pub async fn telegram_webhook_receiver(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(update): Json<super::telegram::Update>,
) -> StatusCode {
    // Look up the channel context in the bridge
    let ctx = match state.telegram_bridge.get_channel_context(channel_id).await {
        Some(ctx) => ctx,
        None => {
            tracing::debug!("Telegram webhook for unknown channel {}", channel_id);
            return StatusCode::NOT_FOUND;
        }
    };

    // Verify the secret token if one was set
    if let Some(ref expected_secret) = ctx.channel.webhook_secret {
        let header_secret = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if header_secret != expected_secret {
            tracing::warn!(
                "Telegram webhook secret mismatch for channel {}",
                channel_id
            );
            return StatusCode::FORBIDDEN;
        }
    }

    // Deduplicate using the channel's own SQLite-backed store (survives restarts).
    // Falls back to in-memory dedup if the mission store is unavailable.
    let is_new = match ctx
        .mission_store
        .register_webhook_update(channel_id, update.update_id)
        .await
    {
        Ok(new) => {
            if new {
                // Keep in-memory map in sync so a later SQLite failure
                // doesn't cause duplicate processing.
                state
                    .telegram_bridge
                    .register_update_once(channel_id, update.update_id)
                    .await;
            }
            new
        }
        Err(_) => {
            // Fallback to in-memory dedup if SQLite fails
            state
                .telegram_bridge
                .register_update_once(channel_id, update.update_id)
                .await
        }
    };
    if !is_new {
        tracing::info!(
            channel_id = %channel_id,
            update_id = update.update_id,
            "Ignoring duplicate Telegram webhook update"
        );
        return StatusCode::OK;
    }

    if let Some(ref msg) = update.message {
        let http = state.telegram_bridge.http().clone();
        super::telegram::process_webhook_message(&ctx, msg, &http, &state.telegram_bridge).await;
    }

    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mission_store::MissionMode;
    use std::sync::Arc;

    fn test_automation_with_mode(
        fresh_session: mission_store::FreshSession,
        variables: HashMap<String, String>,
    ) -> mission_store::Automation {
        mission_store::Automation {
            id: Uuid::new_v4(),
            mission_id: Uuid::new_v4(),
            command_source: mission_store::CommandSource::Inline {
                content: "ping".to_string(),
            },
            trigger: mission_store::TriggerType::AgentFinished,
            variables,
            active: true,
            created_at: mission_store::now_string(),
            last_triggered_at: None,
            retry_config: mission_store::RetryConfig::default(),
            stop_policy: mission_store::StopPolicy::Never,
            fresh_session,
            consecutive_failures: 0,
        }
    }

    #[test]
    fn test_resolve_agent_finished_target_mission_switches_on_completed() {
        let source_mission_id = Uuid::new_v4();
        let next_mission_id = Uuid::new_v4();
        let mut vars = HashMap::new();
        vars.insert("nextSessionId".to_string(), next_mission_id.to_string());
        let automation = test_automation_with_mode(mission_store::FreshSession::Switch, vars);

        let target = resolve_agent_finished_target_mission(
            source_mission_id,
            MissionStatus::Completed,
            &automation,
        );
        assert_eq!(target, next_mission_id);
    }

    #[test]
    fn test_resolve_agent_finished_target_mission_does_not_switch_when_not_completed() {
        let source_mission_id = Uuid::new_v4();
        let next_mission_id = Uuid::new_v4();
        let mut vars = HashMap::new();
        vars.insert("nextSessionId".to_string(), next_mission_id.to_string());
        let automation = test_automation_with_mode(mission_store::FreshSession::Switch, vars);

        let target = resolve_agent_finished_target_mission(
            source_mission_id,
            MissionStatus::Active,
            &automation,
        );
        assert_eq!(target, source_mission_id);
    }

    #[test]
    fn synthetic_thought_from_text_delta_flushes_before_tool_call() {
        let mission_id = Uuid::new_v4();
        let mut pending = HashMap::new();

        let text_delta = AgentEvent::TextDelta {
            content: "Now let me run the build to see how many errors remain.".to_string(),
            mission_id: Some(mission_id),
        };
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &text_delta),
            None
        );

        let tool_call = AgentEvent::ToolCall {
            tool_call_id: "tool-1".to_string(),
            name: "Bash".to_string(),
            args: serde_json::json!({ "command": "lake build" }),
            mission_id: Some(mission_id),
        };
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &tool_call),
            Some("Now let me run the build to see how many errors remain.".to_string())
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn synthetic_thought_from_text_delta_accumulates_incremental_fragments() {
        let mission_id = Uuid::new_v4();
        let mut pending = HashMap::new();

        let first = AgentEvent::TextDelta {
            content: "Now let me run ".to_string(),
            mission_id: Some(mission_id),
        };
        let second = AgentEvent::TextDelta {
            content: "the build.".to_string(),
            mission_id: Some(mission_id),
        };
        let tool_call = AgentEvent::ToolCall {
            tool_call_id: "tool-1".to_string(),
            name: "Bash".to_string(),
            args: serde_json::json!({ "command": "cargo test" }),
            mission_id: Some(mission_id),
        };

        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &first);
        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &second);
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &tool_call),
            Some("Now let me run the build.".to_string())
        );
    }

    #[test]
    fn synthetic_thought_from_text_delta_replaces_accumulated_fragments() {
        let mission_id = Uuid::new_v4();
        let mut pending = HashMap::new();

        let first = AgentEvent::TextDelta {
            content: "Now let me run".to_string(),
            mission_id: Some(mission_id),
        };
        let second = AgentEvent::TextDelta {
            content: "Now let me run the build".to_string(),
            mission_id: Some(mission_id),
        };
        let tool_call = AgentEvent::ToolCall {
            tool_call_id: "tool-1".to_string(),
            name: "Bash".to_string(),
            args: serde_json::json!({ "command": "cargo test" }),
            mission_id: Some(mission_id),
        };

        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &first);
        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &second);
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &tool_call),
            Some("Now let me run the build".to_string())
        );
    }

    #[test]
    fn synthetic_thought_from_text_delta_skips_assistant_echoes() {
        let mission_id = Uuid::new_v4();
        let mut pending = HashMap::new();

        let text_delta = AgentEvent::TextDelta {
            content: "Here is the final answer".to_string(),
            mission_id: Some(mission_id),
        };
        let assistant = AgentEvent::AssistantMessage {
            id: Uuid::new_v4(),
            content: "Here is the final answer with more detail".to_string(),
            success: true,
            cost_cents: 0,
            cost_source: crate::agents::CostSource::Unknown,
            usage: None,
            model: None,
            model_normalized: None,
            mission_id: Some(mission_id),
            shared_files: None,
            resumable: false,
        };

        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &text_delta);
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &assistant),
            None
        );
    }

    #[test]
    fn synthetic_thought_from_text_delta_keeps_tool_narration_before_assistant() {
        let mission_id = Uuid::new_v4();
        let mut pending = HashMap::new();

        let text_delta = AgentEvent::TextDelta {
            content: "I'll inspect the failing theorem before I answer.".to_string(),
            mission_id: Some(mission_id),
        };
        let assistant = AgentEvent::AssistantMessage {
            id: Uuid::new_v4(),
            content: "I found the issue in the induction step.".to_string(),
            success: true,
            cost_cents: 0,
            cost_source: crate::agents::CostSource::Unknown,
            usage: None,
            model: None,
            model_normalized: None,
            mission_id: Some(mission_id),
            shared_files: None,
            resumable: false,
        };

        let _ = synthetic_thought_from_text_delta(&mut pending, mission_id, &text_delta);
        assert_eq!(
            synthetic_thought_from_text_delta(&mut pending, mission_id, &assistant),
            Some("I'll inspect the failing theorem before I answer.".to_string())
        );
    }

    #[tokio::test]
    async fn cleanup_stale_active_missions_once_keeps_recent_active_mission_active() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(
                Some("Claude retry validation"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission should be created");
        store
            .update_mission_status(mission.id, MissionStatus::Active)
            .await
            .expect("mission should become active");

        let (events_tx, _events_rx) = broadcast::channel(8);
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        cleanup_stale_active_missions_once(&store, 24, &events_tx, &cmd_tx).await;

        let stored = store
            .get_mission(mission.id)
            .await
            .expect("mission lookup should succeed")
            .expect("mission should exist");
        assert_eq!(stored.status, MissionStatus::Active);
    }

    #[test]
    fn test_parse_image_tag() {
        let tags = parse_rich_tags(r#"<image path="./chart.png" alt="My Chart" />"#);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag_type, RichTagType::Image);
        assert_eq!(tags[0].path, "./chart.png");
        assert_eq!(tags[0].alt.as_deref(), Some("My Chart"));
    }

    #[test]
    fn test_parse_file_tag() {
        let tags = parse_rich_tags(r#"<file path="./report.pdf" name="Report" />"#);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag_type, RichTagType::File);
        assert_eq!(tags[0].path, "./report.pdf");
        assert_eq!(tags[0].name.as_deref(), Some("Report"));
    }

    #[test]
    fn test_parse_multiple_tags() {
        let content = r#"Here is the chart:
<image path="./a.png" alt="A" />
And the report:
<file path="./b.pdf" name="B" />"#;
        let tags = parse_rich_tags(content);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].tag_type, RichTagType::Image);
        assert_eq!(tags[0].path, "./a.png");
        assert_eq!(tags[1].tag_type, RichTagType::File);
        assert_eq!(tags[1].path, "./b.pdf");
    }

    #[test]
    fn test_parse_no_tags() {
        let tags = parse_rich_tags("Hello world, no tags here.");
        assert!(tags.is_empty());
    }

    #[test]
    fn test_parse_malformed_tag() {
        // Unclosed tag should not match
        let tags = parse_rich_tags(r#"<image path="./chart.png" "#);
        assert!(tags.is_empty());
        // Missing path attribute
        let tags = parse_rich_tags(r#"<image alt="no path" />"#);
        assert!(tags.is_empty());
    }

    #[test]
    fn test_strip_numeric_title_suffix() {
        assert_eq!(
            strip_numeric_title_suffix("Fix flaky CI (2)"),
            "Fix flaky CI"
        );
        assert_eq!(strip_numeric_title_suffix("Fix flaky CI"), "Fix flaky CI");
        assert_eq!(
            strip_numeric_title_suffix("Fix flaky CI (alpha)"),
            "Fix flaky CI (alpha)"
        );
    }

    #[test]
    fn test_has_significant_metadata_drift_detects_minor_and_major_changes() {
        assert!(!has_significant_metadata_drift(
            "Fix the login redirect race",
            "Fix login redirect race"
        ));
        assert!(has_significant_metadata_drift(
            "Fix the login redirect race",
            "Investigate websocket reconnect failures"
        ));
    }

    #[test]
    fn test_has_significant_metadata_drift_handles_unicode_equivalents() {
        assert!(!has_significant_metadata_drift(
            "Исправить сбой входа!",
            "исправить   сбой входа"
        ));
    }

    #[test]
    fn test_is_near_duplicate_title_handles_plural_variant() {
        assert!(is_near_duplicate_title(
            "Fix flaky CI pipeline",
            "Fix flaky CI pipelines"
        ));
    }

    #[test]
    fn test_is_near_duplicate_title_rejects_different_topic() {
        assert!(!is_near_duplicate_title(
            "Investigate API timeout on auth",
            "Refactor dashboard sidebar layout"
        ));
    }

    #[tokio::test]
    async fn test_disambiguate_generated_title_appends_next_suffix() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let existing = store
            .create_mission(Some("Fix flaky CI"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .create_mission(Some("Fix flaky CI (2)"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        let disambiguated = disambiguate_generated_title(&store, existing.id, "Fix flaky CI").await;
        assert_eq!(disambiguated, "Fix flaky CI (3)");
    }

    #[tokio::test]
    async fn test_disambiguate_generated_title_scans_beyond_first_page() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        store
            .create_mission(
                Some("Need unique title"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");

        let mut probe_id = Uuid::nil();
        for idx in 0..205 {
            let mission = store
                .create_mission(
                    Some(&format!("Filler {}", idx)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("create mission");
            if idx == 204 {
                probe_id = mission.id;
            }
        }

        let disambiguated =
            disambiguate_generated_title(&store, probe_id, "Need unique title").await;
        assert_eq!(disambiguated, "Need unique title (2)");
    }

    #[tokio::test]
    async fn test_disambiguate_generated_title_handles_punctuation_only_titles() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let existing = store
            .create_mission(Some("!!!"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .create_mission(Some("!!! (2)"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        let disambiguated = disambiguate_generated_title(&store, existing.id, "!!!").await;
        assert_eq!(disambiguated, "!!! (3)");
    }

    #[tokio::test]
    async fn test_disambiguate_generated_title_handles_unicode_casefolding() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        store
            .create_mission(
                Some("Исправить сбой входа"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        let probe = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");

        let disambiguated =
            disambiguate_generated_title(&store, probe.id, "ИСПРАВИТЬ СБОЙ ВХОДА").await;
        assert_eq!(disambiguated, "ИСПРАВИТЬ СБОЙ ВХОДА (2)");
    }

    #[tokio::test]
    async fn test_disambiguate_generated_title_handles_near_duplicate_variants() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        store
            .create_mission(
                Some("Fix flaky CI pipeline"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        let probe = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");

        let disambiguated =
            disambiguate_generated_title(&store, probe.id, "Fix flaky CI pipelines").await;
        assert_eq!(disambiguated, "Fix flaky CI pipelines (2)");
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_populates_short_description_from_first_message()
    {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let history = vec![("user".to_string(), "Hi".to_string())];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            Some("Hi"),
            false,
        )
        .await;

        assert_eq!(updated_title, None);
        assert_eq!(updated_short_description.as_deref(), Some("Hi"));
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_generates_title_after_first_assistant_reply() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let history = vec![
            (
                "user".to_string(),
                "Investigate oauth callback timeout in production".to_string(),
            ),
            (
                "assistant".to_string(),
                "Investigate oauth callback timeout root cause\nStarting with logs.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Investigate oauth callback timeout root cause")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Investigate oauth callback timeout root cause")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_bootstrap_title_uses_first_assistant_response()
    {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let history = vec![
            (
                "user".to_string(),
                "Investigate oauth callback timeout in production".to_string(),
            ),
            (
                "assistant".to_string(),
                "Initial root cause hypothesis: oauth callback host mismatch.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Follow-up: retry timing also contributes to failures.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Initial root cause hypothesis: oauth callback host mismatch.")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Initial root cause hypothesis: oauth callback host mismatch.")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_bootstrap_uses_first_successful_assistant_response(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let history = vec![
            (
                "user".to_string(),
                "Fix oauth callback failures on mobile safari".to_string(),
            ),
            (
                "assistant".to_string(),
                "Error: unable to complete analysis because logs are missing.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Identified root cause: callback URL host mismatch in production.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Identified root cause: callback URL host mismatch in production.")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Identified root cause: callback URL host mismatch in production.")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_waits_for_successful_assistant_before_bootstrap(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let history = vec![
            (
                "user".to_string(),
                "Fix oauth callback failures on mobile safari".to_string(),
            ),
            (
                "assistant".to_string(),
                "Error: unable to complete analysis because logs are missing.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(updated_title, None);
        assert_eq!(updated_short_description, None);
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_does_not_overwrite_user_managed_title() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Initial title"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                None,
                None,
                Some(Some(METADATA_SOURCE_USER)),
                None,
                None,
            )
            .await
            .expect("mark title as user-managed");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("load mission")
            .expect("mission exists");
        let history = vec![
            ("user".to_string(), "Track flaky auth tests".to_string()),
            (
                "assistant".to_string(),
                "Auth test flakes are caused by parallel DB seed races.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, c)| c.as_str()),
            true,
        )
        .await;

        assert_eq!(updated_title, None);
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Auth test flakes are caused by parallel DB seed races.")
        );
    }

    #[tokio::test]
    async fn test_refresh_mission_metadata_preserves_user_source_when_only_short_description_changes(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(
                Some("User chosen title"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        store
            .update_mission_title(mission.id, "User chosen title")
            .await
            .expect("mark title as user managed");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate oauth callback timeout".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Root cause is stale oauth callback cache state across retries."
                            .to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, _events_rx) = broadcast::channel::<AgentEvent>(16);
        refresh_mission_metadata_for_milestone(&store, &events_tx, mission.id).await;

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(refreshed.title.as_deref(), Some("User chosen title"));
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Root cause is stale oauth callback cache state across retries.")
        );
        assert_eq!(
            refreshed.metadata_source.as_deref(),
            Some(METADATA_SOURCE_USER)
        );
        assert_eq!(
            refreshed.metadata_version.as_deref(),
            Some(METADATA_VERSION_V1)
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_prefers_assistant_short_description_when_title_exists(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(
                Some("Existing mission title"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        let history = vec![
            ("user".to_string(), "Hi".to_string()),
            (
                "assistant".to_string(),
                "Investigate oauth callback timeout root cause and retry behavior.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(updated_title, None);
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Investigate oauth callback timeout root cause and retry behavior.")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_upgrades_existing_short_description_from_assistant(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                None,
                Some(Some("Hi")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                Some(Some(METADATA_VERSION_V1)),
            )
            .await
            .expect("seed short description");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let history = vec![
            ("user".to_string(), "Hi".to_string()),
            (
                "assistant".to_string(),
                "Investigate oauth callback timeout root cause and retry behavior.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Investigate oauth callback timeout root cause and retry behavior.")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Investigate oauth callback timeout root cause and retry behavior.")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_bootstrap_short_description_prefers_first_assistant_even_when_overlap_is_high(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                None,
                Some(Some("Investigate oauth callback timeout root cause")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                Some(Some(METADATA_VERSION_V1)),
            )
            .await
            .expect("seed short description");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let history = vec![
            (
                "user".to_string(),
                "Investigate oauth callback timeout root cause".to_string(),
            ),
            (
                "assistant".to_string(),
                "Investigate oauth callback timeout root cause and retry behavior.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Investigate oauth callback timeout root cause and retry behavior.")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Investigate oauth callback timeout root cause and retry behavior.")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_uses_recent_titles_as_negative_context() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        store
            .create_mission(
                Some("Fix login redirect"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create existing mission");
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create probe mission");
        let history = vec![
            (
                "user".to_string(),
                "Fix login redirect on mobile safari callback flow".to_string(),
            ),
            (
                "assistant".to_string(),
                "Fix login redirect\nI'll investigate auth callback handling.".to_string(),
            ),
        ];

        let (updated_title, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        assert_eq!(
            updated_title.as_deref(),
            Some("Fix login redirect - mobile safari callback flow")
        );
        assert_eq!(
            updated_short_description.as_deref(),
            Some("Fix login redirect")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_negative_context_scans_beyond_first_page() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        store
            .create_mission(
                Some("Fix login redirect"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create existing mission");

        for idx in 0..320 {
            store
                .create_mission(
                    Some(&format!("Filler mission {}", idx)),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("create filler mission");
        }

        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create probe mission");
        let history = vec![
            (
                "user".to_string(),
                "Fix login redirect in admin callback flow for enterprise SSO".to_string(),
            ),
            (
                "assistant".to_string(),
                "Fix login redirect\nI'll investigate auth callback handling.".to_string(),
            ),
        ];

        let (updated_title, _) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;

        let updated_title = updated_title.expect("title should be generated");
        assert_ne!(updated_title, "Fix login redirect");
        assert!(
            updated_title.starts_with("Fix login redirect - admin callback flow"),
            "unexpected diversified title: {updated_title}"
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_refreshes_on_milestone_event() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Legacy title"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Legacy title")),
                Some(Some("Legacy short description")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let history = vec![
            (
                "user".to_string(),
                "Investigate production timeout in payment worker".to_string(),
            ),
            ("assistant".to_string(), "Thanks, checking now.".to_string()),
            (
                "assistant".to_string(),
                "Investigate payment timeout root cause\nStarting with logs.".to_string(),
            ),
        ];

        let without_milestone = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            false,
        )
        .await;
        assert_eq!(without_milestone, (None, None));

        let with_milestone = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            true,
        )
        .await;
        assert_eq!(
            with_milestone.0.as_deref(),
            Some("Investigate payment timeout root cause")
        );
        assert_eq!(
            with_milestone.1.as_deref(),
            Some("Investigate payment timeout root cause")
        );
    }

    #[tokio::test]
    async fn test_generate_mission_metadata_updates_refresh_uses_recent_conversational_short_description(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Legacy title"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Legacy title")),
                Some(Some("Legacy short description")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let history = vec![
            (
                "user".to_string(),
                "Start by auditing invoice retry failures in webhook processing.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Acknowledged. I'll inspect the retry pipeline first.".to_string(),
            ),
            (
                "user".to_string(),
                "Check current retry backoff settings.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Backoff settings retrieved from worker config.".to_string(),
            ),
            (
                "user".to_string(),
                "Look at dead-letter queue behavior next.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Dead-letter queue is receiving retries after max attempts.".to_string(),
            ),
            (
                "user".to_string(),
                "Confirm alerting on repeated failures.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Current alerts only trigger on total outage.".to_string(),
            ),
            (
                "user".to_string(),
                "Propose what we should change.".to_string(),
            ),
            (
                "assistant".to_string(),
                "Finalize webhook retry policy with staged backoff and failure alert routing."
                    .to_string(),
            ),
        ];

        let (_, updated_short_description) = generate_mission_metadata_updates(
            &store,
            mission.id,
            &mission,
            &history,
            history.first().map(|(_, content)| content.as_str()),
            true,
        )
        .await;

        assert_eq!(
            updated_short_description.as_deref(),
            Some("Finalize webhook retry policy with staged backoff and failure alert routing.")
        );
    }

    #[tokio::test]
    async fn test_refresh_mission_metadata_for_milestone_updates_store_and_emits_event() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let model_override = "openai/gpt-5";
        let mission = store
            .create_mission(
                Some("Legacy title"),
                None,
                None,
                Some(model_override),
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Legacy title")),
                Some(Some("Legacy short description")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                None,
            )
            .await
            .expect("seed metadata");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate oauth callback timeout".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content:
                            "Investigate oauth callback timeout root cause\nStarting with ingress logs."
                                .to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        refresh_mission_metadata_for_milestone(&store, &events_tx, mission.id).await;

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.title.as_deref(),
            Some("Investigate oauth callback timeout root cause")
        );
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Investigate oauth callback timeout root cause")
        );
        assert_eq!(
            refreshed.metadata_source.as_deref(),
            Some(METADATA_SOURCE_BACKEND_HEURISTIC)
        );
        assert_eq!(
            refreshed.metadata_version.as_deref(),
            Some(METADATA_VERSION_V1)
        );
        assert_eq!(refreshed.metadata_model.as_deref(), Some(model_override));

        let mut saw_metadata_event = false;
        while let Ok(event) = events_rx.try_recv() {
            if let AgentEvent::MissionMetadataUpdated {
                mission_id,
                metadata_model,
                updated_at,
                ..
            } = event
            {
                if mission_id == mission.id {
                    assert_eq!(metadata_model.as_deref(), Some(model_override));
                    assert_eq!(updated_at.as_deref(), Some(refreshed.updated_at.as_str()));
                    saw_metadata_event = true;
                    break;
                }
            }
        }
        assert!(
            saw_metadata_event,
            "expected mission_metadata_updated event"
        );
    }

    #[tokio::test]
    async fn test_schedule_mission_metadata_refresh_for_milestone_updates_store_and_emits_event() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Legacy title"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Legacy title")),
                Some(Some("Legacy short description")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                None,
            )
            .await
            .expect("seed metadata");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate websocket reconnect loop".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Investigate websocket reconnect loop root cause".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        schedule_mission_metadata_refresh_for_milestone(&store, &events_tx, mission.id);

        let saw_metadata_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events_rx.recv().await {
                    Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                        if mission_id == mission.id =>
                    {
                        break true;
                    }
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        })
        .await
        .expect("timed out waiting for metadata refresh event");
        assert!(
            saw_metadata_event,
            "expected mission_metadata_updated event"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.title.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
    }

    #[tokio::test]
    async fn test_maybe_schedule_mission_metadata_refresh_for_status_forces_terminal_statuses() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Legacy title"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Legacy title")),
                Some(Some("Legacy short description")),
                Some(Some(METADATA_SOURCE_BACKEND_HEURISTIC)),
                None,
                None,
            )
            .await
            .expect("seed metadata");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate websocket reconnect loop".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Checking ingress timeout settings".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Investigate websocket reconnect loop root cause".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        maybe_schedule_mission_metadata_refresh_for_status(
            &store,
            &events_tx,
            mission.id,
            MissionStatus::Completed,
        );

        let saw_metadata_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events_rx.recv().await {
                    Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                        if mission_id == mission.id =>
                    {
                        break true;
                    }
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        })
        .await
        .expect("timed out waiting for metadata refresh event");
        assert!(
            saw_metadata_event,
            "expected mission_metadata_updated event"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.title.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
    }

    #[tokio::test]
    async fn test_maybe_schedule_mission_metadata_refresh_for_status_skips_non_milestone_statuses()
    {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(
                Some("Existing mission title"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Existing mission title")),
                Some(Some("Existing mission short description")),
                None,
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let seeded = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let seeded_updated_at = seeded
            .metadata_updated_at
            .clone()
            .expect("seed metadata timestamp");

        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate websocket reconnect loop".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Investigate websocket reconnect loop root cause".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Collecting additional traces".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        maybe_schedule_mission_metadata_refresh_for_status(
            &store,
            &events_tx,
            mission.id,
            MissionStatus::Active,
        );

        let saw_metadata_event =
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                loop {
                    match events_rx.recv().await {
                        Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                            if mission_id == mission.id =>
                        {
                            break true;
                        }
                        Ok(_) => continue,
                        Err(_) => break false,
                    }
                }
            })
            .await
            .unwrap_or(false);
        assert!(
            !saw_metadata_event,
            "did not expect metadata refresh for non-milestone status"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(refreshed.title.as_deref(), Some("Existing mission title"));
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Existing mission short description")
        );
        assert_eq!(
            refreshed.metadata_updated_at.as_deref(),
            Some(seeded_updated_at.as_str())
        );
    }

    #[tokio::test]
    async fn test_schedule_mission_metadata_refresh_updates_store_without_force_refresh() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate websocket reconnect loop".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Investigate websocket reconnect loop root cause".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        schedule_mission_metadata_refresh(&store, &events_tx, mission.id, false);

        let saw_metadata_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events_rx.recv().await {
                    Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                        if mission_id == mission.id =>
                    {
                        break true;
                    }
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        })
        .await
        .expect("timed out waiting for metadata refresh event");
        assert!(
            saw_metadata_event,
            "expected mission_metadata_updated event"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.title.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Investigate websocket reconnect loop root cause")
        );
    }

    #[tokio::test]
    async fn test_persist_mission_history_and_schedule_metadata_refresh_emits_metadata_update() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        let entries = vec![
            MissionHistoryEntry {
                role: "user".to_string(),
                content: "Debug websocket reconnect loop".to_string(),
            },
            MissionHistoryEntry {
                role: "assistant".to_string(),
                content: "Root cause is stale session token refresh ordering".to_string(),
            },
        ];

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        persist_mission_history_and_schedule_metadata_refresh(
            &store, &events_tx, mission.id, &entries,
        )
        .await;

        let saw_metadata_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match events_rx.recv().await {
                    Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                        if mission_id == mission.id =>
                    {
                        break true;
                    }
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        })
        .await
        .expect("timed out waiting for metadata refresh event");
        assert!(
            saw_metadata_event,
            "expected mission_metadata_updated event"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.title.as_deref(),
            Some("Root cause is stale session token refresh ordering")
        );
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Root cause is stale session token refresh ordering")
        );
    }

    #[tokio::test]
    async fn test_schedule_mission_metadata_refresh_skips_non_cadence_updates_without_force_refresh(
    ) {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Existing mission title")),
                Some(Some("Existing mission short description")),
                None,
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let seeded = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let seeded_updated_at = seeded
            .metadata_updated_at
            .clone()
            .expect("seed metadata timestamp");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Initial request".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Initial response".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Follow-up request".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Refined response".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        schedule_mission_metadata_refresh(&store, &events_tx, mission.id, false);

        let saw_metadata_event =
            tokio::time::timeout(std::time::Duration::from_millis(300), async {
                loop {
                    match events_rx.recv().await {
                        Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                            if mission_id == mission.id =>
                        {
                            break true;
                        }
                        Ok(_) => continue,
                        Err(_) => break false,
                    }
                }
            })
            .await
            .unwrap_or(false);
        assert!(
            !saw_metadata_event,
            "metadata refresh should not run before cadence threshold when force_refresh is false"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(refreshed.title.as_deref(), Some("Existing mission title"));
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Existing mission short description")
        );
        assert_eq!(
            refreshed.metadata_updated_at.as_deref(),
            Some(seeded_updated_at.as_str())
        );
    }

    #[tokio::test]
    async fn test_schedule_mission_metadata_refresh_ignores_non_conversational_entries_for_cadence()
    {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Existing mission title")),
                Some(Some("Existing mission short description")),
                None,
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let seeded = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let seeded_updated_at = seeded
            .metadata_updated_at
            .clone()
            .expect("seed metadata timestamp");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Initial request".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Initial response".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "tool".to_string(),
                        content: "tool_call: inspect logs".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Follow-up request".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Follow-up response".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "tool".to_string(),
                        content: "tool_result: log output".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Investigate retries".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Retries are triggered by 502s".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "Patch retry jitter".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "Added jitter and bounded retries".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        schedule_mission_metadata_refresh(&store, &events_tx, mission.id, false);

        let saw_metadata_event =
            tokio::time::timeout(std::time::Duration::from_millis(300), async {
                loop {
                    match events_rx.recv().await {
                        Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                            if mission_id == mission.id =>
                        {
                            break true;
                        }
                        Ok(_) => continue,
                        Err(_) => break false,
                    }
                }
            })
            .await
            .unwrap_or(false);
        assert!(
            !saw_metadata_event,
            "metadata refresh should not run when only non-conversational entries push history length to cadence boundary"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(refreshed.title.as_deref(), Some("Existing mission title"));
        assert_eq!(
            refreshed.short_description.as_deref(),
            Some("Existing mission short description")
        );
        assert_eq!(
            refreshed.metadata_updated_at.as_deref(),
            Some(seeded_updated_at.as_str())
        );
    }

    #[tokio::test]
    async fn test_schedule_mission_metadata_refresh_uses_last_refresh_baseline_not_global_modulo() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Legacy title"), None, None, None, None, None, None)
            .await
            .expect("create mission");

        let mut history: Vec<MissionHistoryEntry> = Vec::new();
        for idx in 0..27 {
            let role = if idx % 2 == 0 { "user" } else { "assistant" };
            history.push(MissionHistoryEntry {
                role: role.to_string(),
                content: format!("history entry {}", idx),
            });
        }
        store
            .update_mission_history(mission.id, &history)
            .await
            .expect("seed history");

        let (forced_events_tx, _forced_events_rx) = broadcast::channel::<AgentEvent>(16);
        refresh_mission_metadata_for_milestone(&store, &forced_events_tx, mission.id).await;

        let after_forced = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        let forced_timestamp = after_forced
            .metadata_updated_at
            .clone()
            .expect("forced refresh should set metadata timestamp");

        for idx in 27..30 {
            let role = if idx % 2 == 0 { "user" } else { "assistant" };
            history.push(MissionHistoryEntry {
                role: role.to_string(),
                content: format!("post-forced history entry {}", idx),
            });
        }
        store
            .update_mission_history(mission.id, &history)
            .await
            .expect("update history");

        let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(16);
        schedule_mission_metadata_refresh(&store, &events_tx, mission.id, false);

        let saw_metadata_event =
            tokio::time::timeout(std::time::Duration::from_millis(300), async {
                loop {
                    match events_rx.recv().await {
                        Ok(AgentEvent::MissionMetadataUpdated { mission_id, .. })
                            if mission_id == mission.id =>
                        {
                            break true;
                        }
                        Ok(_) => continue,
                        Err(_) => break false,
                    }
                }
            })
            .await
            .unwrap_or(false);
        assert!(
            !saw_metadata_event,
            "metadata refresh should not run only 3 conversational turns after a forced refresh"
        );

        let refreshed = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");
        assert_eq!(
            refreshed.metadata_updated_at.as_deref(),
            Some(forced_timestamp.as_str())
        );
    }

    #[tokio::test]
    async fn test_should_refresh_metadata_by_cadence_rebases_when_history_is_rewritten_shorter() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Existing mission"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Existing mission")),
                Some(Some("Existing short description")),
                None,
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");

        clear_mission_metadata_refresh_state(mission.id);

        assert!(!should_refresh_metadata_by_cadence(
            mission.id, &mission, 24, false
        ));
        assert!(!should_refresh_metadata_by_cadence(
            mission.id, &mission, 4, false
        ));
        assert!(!should_refresh_metadata_by_cadence(
            mission.id, &mission, 13, false
        ));
        assert!(should_refresh_metadata_by_cadence(
            mission.id, &mission, 14, false
        ));

        clear_mission_metadata_refresh_state(mission.id);
    }

    #[tokio::test]
    async fn test_record_metadata_refresh_baseline_from_mission_rebases_manual_title_updates() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Existing mission"), None, None, None, None, None, None)
            .await
            .expect("create mission");
        store
            .update_mission_history(
                mission.id,
                &[
                    MissionHistoryEntry {
                        role: "user".to_string(),
                        content: "one".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "two".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "tool".to_string(),
                        content: "{}".to_string(),
                    },
                    MissionHistoryEntry {
                        role: "assistant".to_string(),
                        content: "three".to_string(),
                    },
                ],
            )
            .await
            .expect("seed history");
        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Existing mission")),
                Some(Some("Existing short description")),
                None,
                None,
                None,
            )
            .await
            .expect("seed metadata");
        let mission = store
            .get_mission(mission.id)
            .await
            .expect("get mission")
            .expect("mission exists");

        clear_mission_metadata_refresh_state(mission.id);
        record_metadata_refresh_baseline(mission.id, 0);

        record_metadata_refresh_baseline_from_mission(mission.id, &mission);

        assert!(!should_refresh_metadata_by_cadence(
            mission.id, &mission, 11, false
        ));
        assert!(!should_refresh_metadata_by_cadence(
            mission.id, &mission, 12, false
        ));
        assert!(should_refresh_metadata_by_cadence(
            mission.id, &mission, 13, false
        ));

        clear_mission_metadata_refresh_state(mission.id);
    }

    #[tokio::test]
    async fn test_register_metadata_refresh_task_replaces_existing_task_for_same_mission() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let first = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let first_task_id = first.id();
        let registration = register_metadata_refresh_task(&mut tasks, mission_id, false, 1, first);
        assert!(registration.superseded.is_none());
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks.get(&mission_id).map(|entry| entry.handle.id()),
            Some(first_task_id)
        );

        let second = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let second_task_id = second.id();
        let registration = register_metadata_refresh_task(&mut tasks, mission_id, false, 2, second);
        let replaced = registration
            .superseded
            .expect("expected previous task to be replaced");
        assert_eq!(replaced.id(), first_task_id);
        replaced.abort();

        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks.get(&mission_id).map(|entry| entry.handle.id()),
            Some(second_task_id)
        );

        if let Some(active) = tasks.remove(&mission_id) {
            active.handle.abort();
        }
    }

    #[tokio::test]
    async fn test_register_metadata_refresh_task_keeps_in_flight_forced_task_over_non_forced() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let forced = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let forced_task_id = forced.id();
        let forced_registration =
            register_metadata_refresh_task(&mut tasks, mission_id, true, 1, forced);
        assert!(forced_registration.superseded.is_none());

        let non_forced = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let non_forced_task_id = non_forced.id();
        let non_forced_registration =
            register_metadata_refresh_task(&mut tasks, mission_id, false, 2, non_forced);
        let rejected = non_forced_registration
            .superseded
            .expect("new non-forced task should be rejected");
        assert_eq!(rejected.id(), non_forced_task_id);
        rejected.abort();

        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks.get(&mission_id).map(|entry| entry.handle.id()),
            Some(forced_task_id)
        );
        assert!(tasks
            .get(&mission_id)
            .is_some_and(|entry| entry.force_refresh));

        if let Some(active) = tasks.remove(&mission_id) {
            active.handle.abort();
        }
    }

    #[tokio::test]
    async fn test_register_metadata_refresh_task_forced_replaces_non_forced() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let non_forced = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let non_forced_task_id = non_forced.id();
        let non_forced_registration =
            register_metadata_refresh_task(&mut tasks, mission_id, false, 1, non_forced);
        assert!(non_forced_registration.superseded.is_none());

        let forced = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let forced_task_id = forced.id();
        let forced_registration =
            register_metadata_refresh_task(&mut tasks, mission_id, true, 2, forced);
        let replaced = forced_registration
            .superseded
            .expect("existing non-forced task should be replaced");
        assert_eq!(replaced.id(), non_forced_task_id);
        replaced.abort();

        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks.get(&mission_id).map(|entry| entry.handle.id()),
            Some(forced_task_id)
        );
        assert!(tasks
            .get(&mission_id)
            .is_some_and(|entry| entry.force_refresh));

        if let Some(active) = tasks.remove(&mission_id) {
            active.handle.abort();
        }
    }

    #[tokio::test]
    async fn test_should_skip_metadata_refresh_schedule_rejects_non_forced_when_forced_in_flight() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let forced = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        let registration = register_metadata_refresh_task(&mut tasks, mission_id, true, 1, forced);
        assert!(registration.superseded.is_none());

        assert!(should_skip_metadata_refresh_schedule(
            &mut tasks, mission_id, false
        ));
        assert!(!should_skip_metadata_refresh_schedule(
            &mut tasks, mission_id, true
        ));

        if let Some(active) = tasks.remove(&mission_id) {
            active.handle.abort();
        }
    }

    #[tokio::test]
    async fn test_should_skip_metadata_refresh_schedule_drops_finished_tasks() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let finished = tokio::spawn(async {});
        let registration =
            register_metadata_refresh_task(&mut tasks, mission_id, true, 1, finished);
        assert!(registration.superseded.is_none());
        tokio::task::yield_now().await;

        assert!(!should_skip_metadata_refresh_schedule(
            &mut tasks, mission_id, false
        ));
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_complete_metadata_refresh_task_only_removes_matching_generation() {
        let mission_id = Uuid::new_v4();
        let mut tasks: HashMap<Uuid, MetadataRefreshTaskEntry> = HashMap::new();

        let task = tokio::spawn(async {});
        let registration = register_metadata_refresh_task(&mut tasks, mission_id, false, 42, task);
        assert!(registration.superseded.is_none());
        assert_eq!(tasks.len(), 1);

        complete_metadata_refresh_task(&mut tasks, mission_id, 41);
        assert_eq!(tasks.len(), 1);

        complete_metadata_refresh_task(&mut tasks, mission_id, 42);
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_clear_mission_metadata_refresh_state_removes_task_and_baseline() {
        let mission_id = Uuid::new_v4();
        let other_mission_id = Uuid::new_v4();

        clear_mission_metadata_refresh_state(mission_id);

        let task = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });

        {
            let mut tasks = MISSION_METADATA_REFRESH_TASKS
                .lock()
                .expect("metadata refresh task registry lock poisoned");
            tasks.insert(
                mission_id,
                MetadataRefreshTaskEntry {
                    handle: task,
                    force_refresh: false,
                    task_id: 1,
                },
            );
            tasks.insert(
                other_mission_id,
                MetadataRefreshTaskEntry {
                    handle: tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }),
                    force_refresh: false,
                    task_id: 2,
                },
            );
        }

        {
            let mut baselines = MISSION_METADATA_REFRESH_BASELINES
                .lock()
                .expect("metadata refresh baseline lock poisoned");
            baselines.insert(mission_id, 10);
            baselines.insert(other_mission_id, 20);
        }

        clear_mission_metadata_refresh_state(mission_id);

        {
            let tasks = MISSION_METADATA_REFRESH_TASKS
                .lock()
                .expect("metadata refresh task registry lock poisoned");
            assert!(!tasks.contains_key(&mission_id));
            assert!(tasks.contains_key(&other_mission_id));
        }
        {
            let baselines = MISSION_METADATA_REFRESH_BASELINES
                .lock()
                .expect("metadata refresh baseline lock poisoned");
            assert!(!baselines.contains_key(&mission_id));
            assert_eq!(baselines.get(&other_mission_id), Some(&20usize));
        }

        clear_mission_metadata_refresh_state(other_mission_id);
    }

    #[tokio::test]
    async fn test_clear_stale_mission_metadata_refresh_state_prunes_deleted_missions() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());

        let deleted_mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create deleted mission");
        let existing_mission = store
            .create_mission(None, None, None, None, None, None, None)
            .await
            .expect("create existing mission");

        let deleted_task = tokio::spawn(std::future::pending::<()>());
        let existing_task = tokio::spawn(std::future::pending::<()>());

        {
            let mut tasks = MISSION_METADATA_REFRESH_TASKS
                .lock()
                .expect("metadata refresh task registry lock poisoned");
            tasks.insert(
                deleted_mission.id,
                MetadataRefreshTaskEntry {
                    task_id: 1,
                    force_refresh: false,
                    handle: deleted_task,
                },
            );
            tasks.insert(
                existing_mission.id,
                MetadataRefreshTaskEntry {
                    task_id: 2,
                    force_refresh: false,
                    handle: existing_task,
                },
            );

            let mut baselines = MISSION_METADATA_REFRESH_BASELINES
                .lock()
                .expect("metadata refresh baseline lock poisoned");
            baselines.insert(deleted_mission.id, 10);
            baselines.insert(existing_mission.id, 20);
        }

        store
            .delete_mission(deleted_mission.id)
            .await
            .expect("delete mission should succeed");

        clear_stale_mission_metadata_refresh_state(&store).await;

        {
            let tasks = MISSION_METADATA_REFRESH_TASKS
                .lock()
                .expect("metadata refresh task registry lock poisoned");
            assert!(!tasks.contains_key(&deleted_mission.id));
            assert!(tasks.contains_key(&existing_mission.id));
        }

        {
            let baselines = MISSION_METADATA_REFRESH_BASELINES
                .lock()
                .expect("metadata refresh baseline lock poisoned");
            assert!(!baselines.contains_key(&deleted_mission.id));
            assert_eq!(baselines.get(&existing_mission.id), Some(&20usize));
        }

        clear_mission_metadata_refresh_state(existing_mission.id);
    }

    #[test]
    fn test_mission_metadata_updated_event_serializes_explicit_null_clears() {
        let mission_id = Uuid::new_v4();
        let event = AgentEvent::MissionMetadataUpdated {
            mission_id,
            title: None,
            short_description: Some("Investigate timeout path".to_string()),
            metadata_updated_at: None,
            updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
        };

        let value = serde_json::to_value(event).expect("serialize mission metadata event");
        assert_eq!(
            value.get("type").and_then(|v| v.as_str()),
            Some("mission_metadata_updated")
        );
        assert!(value.get("title").is_some(), "title key should be present");
        assert!(value.get("title").is_some_and(serde_json::Value::is_null));
        assert_eq!(
            value.get("short_description").and_then(|v| v.as_str()),
            Some("Investigate timeout path")
        );
        assert!(
            value
                .get("metadata_updated_at")
                .is_some_and(serde_json::Value::is_null),
            "metadata_updated_at key should be present as null when cleared"
        );
        assert!(
            value
                .get("updated_at")
                .is_some_and(serde_json::Value::is_null),
            "updated_at key should be present as null when not provided"
        );
    }

    #[test]
    fn test_extract_short_description_from_history_allows_short_messages() {
        let history = vec![("user".to_string(), "Hi".to_string())];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Hi"));
    }

    #[test]
    fn test_extract_short_description_from_history_skips_fenced_code_blocks() {
        let history = vec![(
            "user".to_string(),
            "```rust\nfn main() {}\n```\nInvestigate rust build failure".to_string(),
        )];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Investigate rust build failure"));
    }

    #[test]
    fn test_extract_short_description_from_history_returns_none_for_code_only_message() {
        let history = vec![("user".to_string(), "```bash\necho test\n```".to_string())];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted, None);
    }

    #[test]
    fn test_extract_short_description_from_history_skips_tilde_fenced_code_blocks() {
        let history = vec![(
            "user".to_string(),
            "~~~python\nprint('hi')\n~~~\nDescribe retry strategy".to_string(),
        )];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Describe retry strategy"));
    }

    #[test]
    fn test_extract_short_description_from_history_handles_unclosed_fence() {
        let history = vec![(
            "user".to_string(),
            "```markdown\nInvestigate flaky CI timeout".to_string(),
        )];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Investigate flaky CI timeout"));
    }

    #[test]
    fn test_extract_short_description_from_history_strips_markdown_prefixes() {
        let history = vec![(
            "user".to_string(),
            "## > - * 1. Investigate flaky CI timeout".to_string(),
        )];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Investigate flaky CI timeout"));
    }

    #[test]
    fn test_extract_short_description_from_history_strips_ordered_list_markers() {
        let history = vec![(
            "user".to_string(),
            "12) Resolve dashboard state drift".to_string(),
        )];
        let extracted = extract_short_description_from_history(&history, 160);
        assert_eq!(extracted.as_deref(), Some("Resolve dashboard state drift"));
    }

    #[test]
    fn test_extract_title_from_assistant_skips_fenced_code_blocks() {
        let title = extract_title_from_assistant(
            "```rust\nfn main() {}\n```\nFix flaky CI timeout handling",
        );
        assert_eq!(title.as_deref(), Some("Fix flaky CI timeout handling"));
    }

    #[test]
    fn test_extract_title_from_assistant_returns_none_for_code_only_message() {
        let title = extract_title_from_assistant("```bash\necho test\n```");
        assert_eq!(title, None);
    }

    #[test]
    fn test_extract_title_from_assistant_skips_tilde_fenced_code_blocks() {
        let title =
            extract_title_from_assistant("~~~json\n{\"ok\":true}\n~~~\n# Final status summary");
        assert_eq!(title.as_deref(), Some("Final status summary"));
    }

    #[test]
    fn test_normalize_model_effort_accepts_supported_values() {
        assert_eq!(normalize_model_effort("low"), Some("low".to_string()));
        assert_eq!(
            normalize_model_effort(" Medium "),
            Some("medium".to_string())
        );
        assert_eq!(normalize_model_effort("HIGH"), Some("high".to_string()));
        assert_eq!(normalize_model_effort("xhigh"), Some("xhigh".to_string()));
        assert_eq!(normalize_model_effort("MAX"), Some("max".to_string()));
    }

    #[test]
    fn test_normalize_model_effort_for_backend_rejects_codex_max() {
        assert_eq!(
            normalize_model_effort_for_backend(Some("codex"), "high"),
            Some("high".to_string())
        );
        assert_eq!(
            normalize_model_effort_for_backend(Some("codex"), "max"),
            None
        );
        assert_eq!(
            normalize_model_effort_for_backend(Some("claudecode"), "max"),
            Some("max".to_string())
        );
    }

    #[test]
    fn test_normalize_model_effort_rejects_invalid_values() {
        assert_eq!(normalize_model_effort(""), None);
        assert_eq!(normalize_model_effort("turbo"), None);
    }

    #[test]
    fn test_normalize_model_override_for_backend_keeps_provider_prefix_for_opencode() {
        assert_eq!(
            normalize_model_override_for_backend(Some("opencode"), " openai/gpt-5-codex "),
            Some("openai/gpt-5-codex".to_string())
        );
    }

    #[test]
    fn test_normalize_model_override_for_backend_strips_provider_prefix_for_non_opencode() {
        assert_eq!(
            normalize_model_override_for_backend(Some("codex"), "openai/gpt-5-codex"),
            Some("gpt-5-codex".to_string())
        );
        assert_eq!(
            normalize_model_override_for_backend(Some("claudecode"), "anthropic/claude-opus-4-7"),
            Some("claude-opus-4-7".to_string())
        );
        assert_eq!(
            normalize_model_override_for_backend(Some("codex"), "   "),
            None
        );
    }

    #[test]
    fn test_mission_search_relevance_score_prefers_exact_phrase() {
        let now = mission_store::now_string();
        let strong = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Active,
            title: Some("Fix login timeout regression".to_string()),
            short_description: Some(
                "Investigate timeout happening after login redirect".to_string(),
            ),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "claudecode".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };
        let weak = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Active,
            title: Some("Login failure investigation".to_string()),
            short_description: Some("Issue affecting auth flow and API retries".to_string()),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "claudecode".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };

        let strong_score = mission_search_relevance_score(
            &strong,
            "login timeout",
            strong.workspace_name.as_deref(),
        );
        let weak_score =
            mission_search_relevance_score(&weak, "login timeout", weak.workspace_name.as_deref());

        assert!(strong_score > weak_score);
    }

    #[test]
    fn test_mission_search_relevance_score_supports_query_expansion() {
        let now = mission_store::now_string();
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Completed,
            title: Some("Release prep for dashboard deploy".to_string()),
            short_description: Some("Ship the rollout to production".to_string()),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "codex".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };

        let score = mission_search_relevance_score(
            &mission,
            "deploy rollout",
            mission.workspace_name.as_deref(),
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_mission_search_relevance_score_supports_abbreviation_query_expansion() {
        let now = mission_store::now_string();
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Completed,
            title: Some("Fix session id timeout handling".to_string()),
            short_description: Some("Normalize cookie session id parsing".to_string()),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "codex".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };

        let score = mission_search_relevance_score(
            &mission,
            "sid timeout",
            mission.workspace_name.as_deref(),
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_mission_search_relevance_score_returns_zero_for_non_matching_query() {
        let now = mission_store::now_string();
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Completed,
            title: Some("Implement payment retry flow".to_string()),
            short_description: Some("Handle webhook retries for failed invoices".to_string()),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "codex".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };

        let score = mission_search_relevance_score(
            &mission,
            "kubernetes autoscaling",
            mission.workspace_name.as_deref(),
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_mission_search_relevance_score_ignores_natural_language_stopwords() {
        let now = mission_store::now_string();
        let mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Completed,
            title: Some("Fix session id timeout handling".to_string()),
            short_description: Some("Root cause was stale session id parsing".to_string()),
            metadata_updated_at: None,
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "codex".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };

        let score = mission_search_relevance_score(
            &mission,
            "where did we fix the session id timeout",
            mission.workspace_name.as_deref(),
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_mission_moment_relevance_score_prefers_exact_phrase_match() {
        let exact = mission_moment_relevance_score(
            "assistant",
            "Root cause was a session id timeout in OAuth callback handling.",
            "session id timeout",
        );
        let loose = mission_moment_relevance_score(
            "assistant",
            "Investigating OAuth callback handling and login failures.",
            "session id timeout",
        );
        assert!(exact > loose);
    }

    #[test]
    fn test_mission_moment_relevance_score_returns_zero_for_unrelated_query() {
        let score = mission_moment_relevance_score(
            "assistant",
            "Investigate payment webhook retry loop",
            "kubernetes autoscaling",
        );
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_mission_moment_relevance_score_ignores_natural_language_stopwords() {
        let score = mission_moment_relevance_score(
            "assistant",
            "We fixed the session id timeout by normalizing the cookie key.",
            "show me where we fixed the session id timeout",
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_mission_moment_relevance_score_supports_abbreviation_query_expansion() {
        let score = mission_moment_relevance_score(
            "assistant",
            "We fixed the session id timeout by normalizing the cookie key.",
            "sid timeout",
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_mission_moment_rationale_prefers_phrase_match_with_expansion() {
        let rationale = mission_moment_rationale(
            "assistant",
            "The issue was in session id parsing for cookie state.",
            "sid parsing",
        );
        assert_eq!(rationale, "Phrase match in assistant message");
    }

    #[test]
    fn test_mission_moment_rationale_lists_matched_terms() {
        let rationale = mission_moment_rationale(
            "assistant",
            "The auth flow kept failing because login credentials expired.",
            "why did signin fail",
        );
        assert!(rationale.starts_with("Matched "));
        assert!(rationale.ends_with(" in assistant message"));
        assert!(rationale.contains("fail"));
    }

    #[test]
    fn test_mission_moment_snippet_truncates_utf8_safely() {
        let source = "Fix résumé parser by preserving naïve unicode handling in results";
        let snippet = mission_moment_snippet(source, 18);
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= 19);
    }

    #[test]
    fn test_mission_search_query_hash_normalizes_equivalent_queries() {
        let hash_a = mission_search_query_hash("Login Timeout");
        let hash_b = mission_search_query_hash(" login   timeout ");
        let hash_c = mission_search_query_hash("LOGIN-timeout");
        assert_eq!(hash_a, hash_b);
        assert_eq!(hash_b, hash_c);
    }

    #[test]
    fn test_mission_search_freshness_key_changes_on_metadata_update() {
        let now = mission_store::now_string();
        let mut mission = Mission {
            id: Uuid::new_v4(),
            status: MissionStatus::Completed,
            title: Some("Implement payment retry flow".to_string()),
            short_description: Some("Handle webhook retries for failed invoices".to_string()),
            metadata_updated_at: Some(now.clone()),
            metadata_source: None,
            metadata_model: None,
            metadata_version: None,
            workspace_id: crate::workspace::DEFAULT_WORKSPACE_ID,
            workspace_name: Some("Sandboxed".to_string()),
            agent: None,
            model_override: None,
            model_effort: None,
            backend: "codex".to_string(),
            config_profile: None,
            history: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            interrupted_at: None,
            resumable: false,
            desktop_sessions: Vec::new(),
            session_id: None,
            terminal_reason: None,
            parent_mission_id: None,
            working_directory: None,
            mission_mode: MissionMode::default(),
            goal_mode: false,
            goal_objective: None,
        };
        let before = mission_search_freshness_key(
            &[MissionSearchCandidate {
                mission: mission.clone(),
                relevance_score: 1.0,
            }],
            100,
        );

        mission.short_description = Some("Handle webhook retries and alerting".to_string());
        mission.metadata_updated_at = Some("2099-01-01T00:00:00Z".to_string());
        let after = mission_search_freshness_key(
            &[MissionSearchCandidate {
                mission,
                relevance_score: 1.0,
            }],
            100,
        );

        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn test_mission_search_recency_fingerprint_changes_on_metadata_update() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(
                Some("Investigate cache drift"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("mission should be created");

        let before = mission_search_recency_fingerprint(&store)
            .await
            .expect("recency fingerprint should be computed");

        store
            .update_mission_metadata(
                mission.id,
                Some(Some("Investigate cache drift")),
                Some(Some("Updated metadata")),
                None,
                None,
                None,
            )
            .await
            .expect("metadata update should succeed");

        let after = mission_search_recency_fingerprint(&store)
            .await
            .expect("recency fingerprint should be computed");
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn test_mission_search_recency_fingerprint_changes_for_non_head_mission_update() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let older_mission = store
            .create_mission(Some("Older mission"), None, None, None, None, None, None)
            .await
            .expect("older mission should be created");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let newer_mission = store
            .create_mission(Some("Newer mission"), None, None, None, None, None, None)
            .await
            .expect("newer mission should be created");

        let before = mission_search_recency_fingerprint(&store)
            .await
            .expect("recency fingerprint should be computed");

        store
            .update_mission_metadata(
                older_mission.id,
                Some(Some("Older mission")),
                Some(Some("Metadata changed on older mission")),
                None,
                None,
                None,
            )
            .await
            .expect("metadata update should succeed");

        // Ensure older_mission is non-head at read time.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        store
            .update_mission_title(newer_mission.id, "Newer mission touched")
            .await
            .expect("title update should succeed");

        let after = mission_search_recency_fingerprint(&store)
            .await
            .expect("recency fingerprint should be computed");
        assert_ne!(before, after);
    }

    #[test]
    fn test_mission_search_cache_hit_by_freshness_requires_recency_match() {
        let entry = MissionSearchCacheEntry {
            cached_at: std::time::Instant::now(),
            freshness_key: 42,
            recency_fingerprint: 100,
            results: Vec::new(),
        };

        assert!(mission_search_cache_hit_by_freshness(&entry, 100, 42));
        assert!(!mission_search_cache_hit_by_freshness(&entry, 101, 42));
    }

    #[test]
    fn test_automation_library_command_body_strips_frontmatter() {
        let content = r#"---
description: Analyze failures
params: [service]
---

Investigate <service/> failures.
"#;
        assert_eq!(
            automation_library_command_body(content),
            "Investigate <service/> failures."
        );
    }

    #[test]
    fn test_automation_library_command_body_without_frontmatter() {
        assert_eq!(
            automation_library_command_body("  Echo current status. \n"),
            "Echo current status."
        );
    }

    #[test]
    fn test_enqueue_agent_finished_messages_appends_after_existing_queue_items() {
        let existing_target = Uuid::new_v4();
        let restart_target = Uuid::new_v4();
        let mut queue = VecDeque::from([(
            Uuid::new_v4(),
            "queued user message".to_string(),
            None,
            Some(existing_target),
        )]);

        enqueue_agent_finished_messages(
            &mut queue,
            vec![
                RoutedAutomationMessage {
                    content: "restart 1".to_string(),
                    target_mission_id: restart_target,
                },
                RoutedAutomationMessage {
                    content: "restart 2".to_string(),
                    target_mission_id: restart_target,
                },
            ],
        );

        let ordered: Vec<(String, Option<Uuid>)> = queue
            .into_iter()
            .map(|(_, content, _, mission_id)| (content, mission_id))
            .collect();
        assert_eq!(
            ordered,
            vec![
                ("queued user message".to_string(), Some(existing_target)),
                ("restart 1".to_string(), Some(restart_target)),
                ("restart 2".to_string(), Some(restart_target)),
            ]
        );
    }

    #[test]
    fn test_queue_has_pending_target_mission_matches_same_mission_only() {
        let mission_id = Uuid::new_v4();
        let other_mission_id = Uuid::new_v4();
        let queue = VecDeque::from([
            (
                Uuid::new_v4(),
                "other mission".to_string(),
                None,
                Some(other_mission_id),
            ),
            (
                Uuid::new_v4(),
                "current mission".to_string(),
                None,
                Some(mission_id),
            ),
        ]);

        assert!(queue_has_pending_target_mission(&queue, mission_id));
        assert!(!queue_has_pending_target_mission(&queue, Uuid::new_v4()));
    }

    #[test]
    fn test_mission_status_for_terminal_reason_defers_turn_complete_until_idle_finalization() {
        assert_eq!(
            mission_status_for_terminal_reason(TerminalReason::TurnComplete, false),
            None
        );
        assert_eq!(
            mission_status_for_terminal_reason(TerminalReason::TurnComplete, true),
            Some((MissionStatus::Completed, "completed"))
        );
    }

    #[tokio::test]
    async fn maybe_finalize_terminal_mission_preserves_interrupted_status() {
        let store: Arc<dyn MissionStore> = Arc::new(mission_store::InMemoryMissionStore::new());
        let mission = store
            .create_mission(Some("Shutdown race"), None, None, None, None, None, None)
            .await
            .expect("mission should be created");
        store
            .update_mission_status(mission.id, MissionStatus::Interrupted)
            .await
            .expect("mission should be interrupted");
        let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(8);

        maybe_finalize_terminal_mission(
            &store,
            &events_tx,
            mission.id,
            Some(TerminalReason::LlmError),
            false,
            "shutdown race test",
        )
        .await;

        let updated = store
            .get_mission(mission.id)
            .await
            .expect("mission lookup should succeed")
            .expect("mission should exist");
        assert_eq!(updated.status, MissionStatus::Interrupted);
        assert_eq!(updated.terminal_reason, None);
        assert!(updated.resumable);
        assert!(events_rx.try_recv().is_err());
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_bare_internal_server_error() {
        let mut result =
            crate::agents::AgentResult::failure("Internal server error".to_string(), 0)
                .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_missing_claude_credentials() {
        let mut result = crate::agents::AgentResult::failure(
            "No Claude Code credentials detected. Either run `claude /login` on the host, or authenticate in Settings → AI Providers / set CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY."
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_structured_codex_model_error() {
        let mut result = crate::agents::AgentResult::failure(
            r#"{"detail":"The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT account."}"#
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_provider_payload_error() {
        let mut result = crate::agents::AgentResult::failure(
            "messages.13.content.88.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: 2000 pixels"
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_claude_cli_401() {
        // Regression: Claude Code CLI prints this exact string when
        // Anthropic 401s mid-turn. It was previously upgraded to
        // TurnComplete because the output exceeded 20 chars and
        // didn't match a known bare-error prefix — users then saw the
        // auth error as a successful assistant reply.
        let mut result = crate::agents::AgentResult::failure(
            "Failed to authenticate. API Error: 401 terminated".to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::AuthError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::AuthError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_generic_api_error_401() {
        // Any short output whose substantive content is an HTTP 401
        // status from the underlying provider should stay classified
        // as AuthError regardless of the leading prefix.
        let mut result = crate::agents::AgentResult::failure(
            "Anthropic returned an error. API Error: 401 Unauthorized".to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::AuthError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::AuthError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_codex_process_exit() {
        let mut result = crate::agents::AgentResult::failure(
            "Codex CLI exited before completing the turn (exit_status: signal: 9 (SIGKILL)). Stderr: <empty> | Stdout: <empty>"
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_recovers_substantive_output() {
        let mut result = crate::agents::AgentResult::failure(
            "I completed the implementation and verified the focused build.".to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::TurnComplete));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_claude_transport_failure() {
        let mut result = crate::agents::AgentResult::failure(
            "Claude Code produced no stream events after startup timeout. \
             The Claude CLI started but did not emit any stream-json events."
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError)
        .with_data(serde_json::json!({
            "claudecode_transport_failure": {
                "stage": "startup",
                "idle_timeout_triggered": false,
                "process_exited_without_result": false,
                "pending_tool_names": [],
            }
        }));

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[test]
    fn maybe_recover_soft_llm_error_does_not_recover_claude_startup_timeout_message() {
        let mut result = crate::agents::AgentResult::failure(
            "Claude Code produced no stream events after startup timeout. \
             More text that pushes it past the 20 char heuristic."
                .to_string(),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);

        maybe_recover_soft_llm_error(&mut result);

        assert!(!result.success);
        assert_eq!(result.terminal_reason, Some(TerminalReason::LlmError));
    }

    #[tokio::test]
    async fn test_stop_policy_matches_consecutive_failures() {
        assert!(
            stop_policy_matches_status(
                &mission_store::StopPolicy::WhenFailingConsecutively { count: 2 },
                MissionStatus::Failed,
                2
            )
            .await
        );
        assert!(
            !stop_policy_matches_status(
                &mission_store::StopPolicy::WhenFailingConsecutively { count: 2 },
                MissionStatus::Failed,
                1
            )
            .await
        );
        assert!(
            stop_policy_matches_status(
                &mission_store::StopPolicy::WhenFailingConsecutively { count: 3 },
                MissionStatus::Failed,
                3
            )
            .await
        );
        assert!(
            !stop_policy_matches_status(
                &mission_store::StopPolicy::WhenFailingConsecutively { count: 3 },
                MissionStatus::Failed,
                2
            )
            .await
        );
    }

    #[tokio::test]
    async fn test_stop_policy_never_never_matches() {
        assert!(
            !stop_policy_matches_status(
                &mission_store::StopPolicy::Never,
                MissionStatus::Completed,
                0
            )
            .await
        );
        assert!(
            !stop_policy_matches_status(
                &mission_store::StopPolicy::Never,
                MissionStatus::Failed,
                5
            )
            .await
        );
    }

    #[tokio::test]
    async fn test_validate_rich_tags_resolves_relative_and_blocks_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let good_path = root.join("chart.png");
        tokio::fs::write(&good_path, b"pngbytes").await.unwrap();

        // Should resolve ./chart.png within working_dir.
        let tags = parse_rich_tags(r#"<image path="./chart.png" alt="Chart" />"#);
        let files = validate_rich_tags(&tags, root, None, None).await;
        assert_eq!(files.len(), 1);
        assert!(files[0].url.contains("path="));

        // Create a file outside working_dir and ensure traversal is blocked.
        let parent = root.parent().expect("parent dir exists");
        let evil_path = parent.join(format!("evil-{}.txt", Uuid::new_v4()));
        tokio::fs::write(&evil_path, b"nope").await.unwrap();

        let tags = parse_rich_tags(&format!(
            r#"<file path="../{}" name="Evil" />"#,
            evil_path.file_name().unwrap().to_string_lossy()
        ));
        let files = validate_rich_tags(&tags, root, None, None).await;
        assert!(files.is_empty());

        let tags = parse_rich_tags(&format!(
            r#"<file path="{}" name="EvilAbs" />"#,
            evil_path.to_string_lossy()
        ));
        let files = validate_rich_tags(&tags, root, None, None).await;
        assert!(files.is_empty());
    }
}
