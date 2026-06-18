//! Codex CLI turn runner (app-server driver + ChatGPT account rotation).
//!
//! Moved verbatim from `mission_runner.rs` (Phase 2 of the decomposition).
//! Account/credential plumbing (cooldowns, leasing, CLI bootstrap) still
//! lives in `mission_runner` and is consumed via `pub(crate)` items.

use std::collections::HashSet;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agents::{AgentResult, CompletionConfidence, CompletionSignal, TerminalReason};
use crate::api::control::AgentEvent;
use crate::api::mission_runner::*;
use crate::cost::resolve_cost_cents_and_source;
use crate::workspace::Workspace;
use crate::workspace_exec::WorkspaceExec;

pub(crate) fn codex_turn_requires_tool_activity(
    user_message: &str,
    assistant_message: &str,
) -> bool {
    let user_request = current_user_request_for_tool_activity(user_message);
    let user = user_request.to_ascii_lowercase();
    let assistant = assistant_message.trim().to_ascii_lowercase();

    let deferred_action_prefixes = [
        "i'll perform",
        "i’ll perform",
        "i will perform",
        "i'll run",
        "i’ll run",
        "i will run",
        "i'll execute",
        "i’ll execute",
        "i will execute",
        "i'll create",
        "i’ll create",
        "i will create",
        "i'll inspect",
        "i’ll inspect",
        "i will inspect",
        "i'll review",
        "i’ll review",
        "i will review",
    ];
    if deferred_action_prefixes
        .iter()
        .any(|prefix| assistant.starts_with(prefix))
    {
        return true;
    }

    // Advisory prompts ("how do I run tests?", "explain what cargo does")
    // contain verbs like "run" or "test" but don't ask us to execute them.
    // If we classified those as tool-required, a perfectly good text-only
    // answer from Codex would get converted into a `Stalled` failure.
    //
    // Mixed prompts like "How do I run these tests? Please run them and
    // fix failures." still request execution; the advisory heuristic
    // must not bypass the imperative half. Only short-circuit when no
    // explicit imperative follow-up is present.
    if user_looks_advisory(&user) && !user_has_imperative_execution_request(&user) {
        return false;
    }

    let explicit_tool_markers = [
        "```bash",
        "shell command",
        "using shell",
        "run ",
        " run ",
        "execute ",
        " execute ",
        "test ",
        " test ",
        "debug ",
        " debug ",
        "fix ",
        " fix ",
        "implement ",
        " implement ",
        "edit ",
        " edit ",
        "modify ",
        " modify ",
        "inspect ",
        " inspect ",
        "search ",
        " search ",
        " grep ",
        " rg ",
        " ls ",
        " cat ",
        " wc ",
        " curl ",
        " git ",
        " npm ",
        " bun ",
        " cargo ",
        " python ",
        " pytest ",
    ];
    if explicit_tool_markers
        .iter()
        .any(|marker| user.contains(marker))
    {
        return true;
    }

    let action_markers = [
        "create", "write", "read", "open", "access", "review", "inspect", "check", "update",
        "change", "debug", "fix",
    ];
    let object_markers = [
        " file",
        " files",
        " directory",
        " folder",
        " workspace",
        " pull request",
        " pr #",
        " github.com/",
        ".rs",
        ".ts",
        ".tsx",
        ".js",
        ".json",
        ".toml",
        ".md",
        ".pdf",
        "http://",
        "https://",
        "localhost",
    ];

    action_markers
        .iter()
        .any(|action| contains_ascii_word(&user, action))
        && object_markers.iter().any(|object| user.contains(object))
}

pub(crate) fn codex_is_goal_request(user_message: &str) -> bool {
    user_message.trim_start().starts_with("/goal ")
}

pub(crate) fn codex_missing_goal_final_response_message() -> String {
    "Goal completed, but Codex did not emit a final assistant response. The last reasoning block was captured in the thinking panel, but it is not being promoted to the completion message."
        .to_string()
}

/// Does the user message read as a question or request-for-explanation,
/// rather than an imperative "go do this"? Used to suppress the
/// `explicit_tool_markers` heuristic so advisory questions that mention
/// common verbs ("how do I run tests", "explain cargo") don't get
/// mis-classified as tool-required.
fn user_looks_advisory(user_lower: &str) -> bool {
    let trimmed = user_lower.trim_start();
    const ADVISORY_PREFIXES: &[&str] = &[
        "how do i ",
        "how do you ",
        "how to ",
        "how can i ",
        "how does ",
        "how should ",
        "how would ",
        "how is ",
        "how are ",
        "what is ",
        "what are ",
        "what does ",
        "what do ",
        "what would ",
        "what happens ",
        "what's ",
        "why does ",
        "why is ",
        "why are ",
        "why do ",
        "when should ",
        "when does ",
        "when do ",
        "where does ",
        "where is ",
        "where are ",
        "explain ",
        "describe ",
        "summarize ",
        "tell me about ",
        "tell me how ",
        "tell me why ",
        "can you explain ",
        "can you describe ",
        "could you explain ",
        "would you explain ",
    ];
    ADVISORY_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Detects explicit imperative execution requests that override the
/// advisory heuristic. Input is expected to be ASCII-lowercased.
///
/// Entries must be **unambiguous** — they should never match a purely
/// explanatory question. Phrases like `run this` / `run it` are not
/// safe to include (they appear inside questions such as "How do I
/// run this locally?"); rely on explicit imperative framing
/// (`please`, `actually`, `go ahead`, `then`, `now`) or on
/// direct-object coupling with verbs that can't occur mid-question
/// without being a command (`fix failures`, `apply the fix`).
fn user_has_imperative_execution_request(user_lower: &str) -> bool {
    const IMPERATIVE_PHRASES: &[&str] = &[
        // Explicit politeness prefix — only present when the user is
        // directing us to act.
        "please run",
        "please execute",
        "please apply",
        "please fix",
        "please implement",
        "please do ",
        // "Actually" framing is also unambiguous: "actually run" only
        // shows up as a follow-up command.
        "actually run",
        "actually execute",
        "go ahead and ",
        // Sequencing markers — if the user says "then run" or "now
        // run" after a question, they're asking us to do it next.
        "then run",
        "then execute",
        "now run",
        "now execute",
        "and run them",
        "and execute them",
        "and fix",
        // Direct-object phrases that don't fit neatly inside an
        // advisory question.
        "run the tests",
        "fix failures",
        "fix the failures",
        "apply the fix",
    ];
    IMPERATIVE_PHRASES
        .iter()
        .any(|phrase| user_lower.contains(phrase))
}

pub(crate) fn codex_final_message_looks_like_progress_update(assistant_message: &str) -> bool {
    let assistant = assistant_message.trim().to_ascii_lowercase();
    if assistant.is_empty() {
        return false;
    }

    let progress_prefixes = [
        "i'm reading",
        "i’m reading",
        "i am reading",
        "i'm checking",
        "i’m checking",
        "i am checking",
        "i'm inspecting",
        "i’m inspecting",
        "i am inspecting",
        "i'm pulling",
        "i’m pulling",
        "i am pulling",
        "i'm running",
        "i’m running",
        "i am running",
        "i'll run",
        "i’ll run",
        "i will run",
        "i'll execute",
        "i’ll execute",
        "i will execute",
        "next i'm",
        "next i’m",
        "next i'll",
        "next i’ll",
        "now i'm",
        "now i’m",
    ];
    if progress_prefixes
        .iter()
        .any(|prefix| assistant.starts_with(prefix))
    {
        return true;
    }

    assistant.contains(" i'm reading ")
        || assistant.contains(" i’m reading ")
        || assistant.contains(" i'm checking ")
        || assistant.contains(" i’m checking ")
        || assistant.contains(" i'm running ")
        || assistant.contains(" i’m running ")
}

fn current_user_request_for_tool_activity(prompt: &str) -> &str {
    let Some((_, after_user)) = prompt.rsplit_once("User:\n") else {
        return prompt;
    };
    after_user
        .split_once("\n\nInstructions:")
        .map(|(current, _)| current)
        .unwrap_or(after_user)
}

fn contains_ascii_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if haystack.len() < needle.len() {
        return false;
    }
    for idx in 0..=haystack.len() - needle.len() {
        if &haystack[idx..idx + needle.len()] != needle {
            continue;
        }
        let before = idx.checked_sub(1).and_then(|prev| haystack.get(prev));
        let after = haystack.get(idx + needle.len());
        if before.is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
            && after.is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        {
            return true;
        }
    }
    false
}

/// Pull the "try again at <when>" reset window Codex appends to its usage-limit
/// message, e.g. `Jun 11th, 2026 3:00 AM`. Returns the raw human string (without
/// the trailing period) so it can be shown verbatim, since Codex does not
/// include the timezone.
pub(crate) fn extract_codex_reset_window(output: &str) -> Option<String> {
    // The message is ASCII, so a case-insensitive byte search keeps offsets
    // aligned with `output`.
    let lower = output.to_ascii_lowercase();
    let marker = "try again at ";
    let start = lower.find(marker)? + marker.len();
    let rest = &output[start..];
    // Codex ends the sentence with a period; stop at that (or a newline).
    let end = rest.find(['.', '\n']).unwrap_or(rest.len());
    let window = rest[..end].trim();
    (!window.is_empty()).then(|| window.to_string())
}

/// Best-effort parse of a Codex reset window into a comparable timestamp so the
/// aggregated message can report the *earliest* reset across accounts. Returns
/// `None` (and the caller falls back to display order) when the format drifts.
fn parse_codex_reset_window(window: &str) -> Option<chrono::NaiveDateTime> {
    // Drop the ordinal suffix that always precedes the comma ("11th," -> "11,").
    let cleaned = window
        .replace("th,", ",")
        .replace("st,", ",")
        .replace("nd,", ",")
        .replace("rd,", ",");
    for fmt in ["%b %d, %Y %I:%M %p", "%b %e, %Y %I:%M %p"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(cleaned.trim(), fmt) {
            return Some(dt);
        }
    }
    None
}

/// Build a single user-facing message for the case where rotation tried every
/// connected Codex account and they were *all* at their ChatGPT usage limit.
/// Without this the runner just echoes the last account's raw "try again at …"
/// line, which is identical to the first account's and reads as if no rotation
/// happened at all.
pub(crate) fn summarize_codex_usage_caps(
    capped_outputs: &[String],
    account_count: usize,
) -> String {
    // Distinct reset windows, soonest first when parseable.
    let mut windows: Vec<(Option<chrono::NaiveDateTime>, String)> = Vec::new();
    for output in capped_outputs {
        if let Some(window) = extract_codex_reset_window(output) {
            if !windows.iter().any(|(_, existing)| existing == &window) {
                windows.push((parse_codex_reset_window(&window), window));
            }
        }
    }
    windows.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    let mut msg = format!(
        "All {} connected Codex account{} are at their ChatGPT usage limit.",
        account_count,
        if account_count == 1 { "" } else { "s" }
    );
    match windows.split_first() {
        Some(((_, earliest), [])) => {
            msg.push_str(&format!(" Usage resets at {earliest}."));
        }
        Some(((_, earliest), _rest)) => {
            msg.push_str(&format!(" Earliest reset at {earliest}."));
        }
        None => {}
    }
    msg.push_str(
        " Connect a Codex account with available quota (or an OpenAI API key), \
         switch this mission to another backend, or wait for the reset.",
    );
    msg
}

/// Run a codex turn through the unified credential pool with rotation and
/// account-level cooldown handling. Shared by the initial mission dispatch
/// and the control-channel follow-up path so a usage-capped ChatGPT account
/// rotates to the next credential everywhere.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_codex_turn_with_rotation(
    workspace: &Workspace,
    mission_work_dir: &std::path::Path,
    codex_message: &str,
    requested_model: Option<&str>,
    model_effort: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    session_id: Option<&str>,
) -> AgentResult {
    'codex_arm: {
        let mut all_creds = collect_codex_credentials(app_working_dir);
        let mut prior_empty_result: Option<AgentResult> = None;
        if all_creds.is_empty() {
            let mut result = run_codex_turn(
                workspace,
                mission_work_dir,
                codex_message,
                requested_model,
                model_effort,
                agent,
                mission_id,
                events_tx.clone(),
                cancel.clone(),
                app_working_dir,
                session_id,
                None,
            )
            .await;

            if let Some(fallback_model) =
                codex_chatgpt_fallback_for_result(requested_model, &result)
            {
                tracing::warn!(
                    mission_id = %mission_id,
                    requested_model = ?requested_model,
                    fallback_model,
                    "Retrying Codex turn with fallback model for ChatGPT account compatibility"
                );
                result = run_codex_turn(
                    workspace,
                    mission_work_dir,
                    codex_message,
                    Some(fallback_model),
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    app_working_dir,
                    session_id,
                    None,
                )
                .await;
            } else if codex_tool_stall_should_retry_with_default_model(requested_model, &result) {
                tracing::warn!(
                    mission_id = %mission_id,
                    requested_model = ?requested_model,
                    "Retrying Codex turn on the requested model (not the stale Codex CLI default) after it stopped before tool use"
                );
                result = run_codex_turn(
                    workspace,
                    mission_work_dir,
                    codex_message,
                    // Was `None`, which made the Codex CLI fall back to
                    // its built-in default — currently the retired
                    // `gpt-5.3-codex`, rejected by ChatGPT-account auth
                    // with a 400. Retry on the requested (latest) model.
                    requested_model,
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    app_working_dir,
                    session_id,
                    None,
                )
                .await;
            }

            // Defensive re-query: if this turn was rate/capacity limited
            // and a fresh enumeration now returns accounts, fall through
            // to the rotation loop instead of surfacing the failure.
            let constrained = matches!(
                result.terminal_reason,
                Some(TerminalReason::RateLimited | TerminalReason::CapacityLimited)
            );
            if constrained {
                let recheck = collect_codex_credentials(app_working_dir);
                if !recheck.is_empty() {
                    tracing::warn!(
                        mission_id = %mission_id,
                        recovered_credentials = recheck.len(),
                        "Codex credential pool was empty on first attempt but re-query found accounts after a rate-limited turn; retrying with rotation"
                    );
                    all_creds = recheck;
                    prior_empty_result = Some(result);
                    // fall through to rotation loop below
                } else {
                    break 'codex_arm result;
                }
            } else {
                break 'codex_arm result;
            }
        }
        {
            let mut attempted_credentials: HashSet<String> = HashSet::new();
            let mut attempt_idx = 0usize;
            // Raw outputs of attempts that failed specifically on a usage cap,
            // so an exhausted pool can be summarized instead of echoing the
            // last account's bare "try again at …" message.
            let mut usage_capped_outputs: Vec<String> = Vec::new();
            let mut last_constrained_result: Option<AgentResult> = prior_empty_result;

            loop {
                if cancel.is_cancelled() {
                    break last_constrained_result.unwrap_or_else(cancel_or_shutdown_failure);
                }

                let lease =
                    lease_codex_account(app_working_dir, &attempted_credentials, &cancel).await;
                let Some(lease) = lease else {
                    if let Some(prev) = last_constrained_result {
                        break prev;
                    }
                    break AgentResult::failure(
                    "All configured Codex accounts are currently at capacity. Try again shortly."
                        .to_string(),
                    0,
                )
                .with_terminal_reason(TerminalReason::CapacityLimited);
                };

                attempt_idx += 1;
                let credential_label = lease.credential.label_for_logs();
                let credential_fingerprint = lease.credential.fingerprint();
                attempted_credentials.insert(credential_fingerprint.clone());
                let credential_override = lease.credential.as_override();

                tracing::info!(
                    mission_id = %mission_id,
                    attempt = attempt_idx,
                    credential = %credential_label,
                    total_credentials = all_creds.len(),
                    "Running Codex turn with leased account slot"
                );

                let mut result = run_codex_turn(
                    workspace,
                    mission_work_dir,
                    codex_message,
                    requested_model,
                    model_effort,
                    agent,
                    mission_id,
                    events_tx.clone(),
                    cancel.clone(),
                    app_working_dir,
                    session_id,
                    Some(&credential_override),
                )
                .await;

                if let Some(fallback_model) =
                    codex_chatgpt_fallback_for_result(requested_model, &result)
                {
                    tracing::warn!(
                        mission_id = %mission_id,
                        attempt = attempt_idx,
                        requested_model = ?requested_model,
                        fallback_model,
                        credential = %credential_label,
                        "Retrying Codex turn with fallback model for ChatGPT account compatibility"
                    );
                    result = run_codex_turn(
                        workspace,
                        mission_work_dir,
                        codex_message,
                        Some(fallback_model),
                        model_effort,
                        agent,
                        mission_id,
                        events_tx.clone(),
                        cancel.clone(),
                        app_working_dir,
                        session_id,
                        Some(&credential_override),
                    )
                    .await;
                } else if codex_tool_stall_should_retry_with_default_model(requested_model, &result)
                {
                    tracing::warn!(
                        mission_id = %mission_id,
                        attempt = attempt_idx,
                        requested_model = ?requested_model,
                        credential = %credential_label,
                        "Retrying Codex turn on the requested model (not the stale Codex CLI default) after it stopped before tool use"
                    );
                    result = run_codex_turn(
                        workspace,
                        mission_work_dir,
                        codex_message,
                        // Was `None` (stale CLI default gpt-5.3-codex,
                        // 400 on ChatGPT auth). Retry on the requested
                        // (latest) model instead.
                        requested_model,
                        model_effort,
                        agent,
                        mission_id,
                        events_tx.clone(),
                        cancel.clone(),
                        app_working_dir,
                        session_id,
                        Some(&credential_override),
                    )
                    .await;
                }

                // Cooldown bookkeeping: a capped/constrained account is
                // skipped by every future lease (this turn or any later
                // path) until its cooldown lapses; a healthy turn clears
                // any stale cooldown.
                match result
                    .terminal_reason
                    .as_ref()
                    .and_then(codex_cooldown_for_reason)
                {
                    Some(cooldown) => set_codex_account_cooldown(&credential_fingerprint, cooldown),
                    None => clear_codex_account_cooldown(&credential_fingerprint),
                }
                drop(lease);

                // Record usage-cap failures so an all-capped pool can be
                // summarized. Auth errors rotate too but aren't usage caps,
                // so they're deliberately excluded from this tally.
                if matches!(
                    result.terminal_reason,
                    Some(TerminalReason::RateLimited | TerminalReason::CapacityLimited)
                ) {
                    usage_capped_outputs.push(result.output.clone());
                }

                match result.terminal_reason {
                    Some(
                        TerminalReason::RateLimited
                        | TerminalReason::CapacityLimited
                        | TerminalReason::AuthError,
                    ) if attempted_credentials.len() < all_creds.len() => {
                        let reason = match result.terminal_reason {
                            Some(TerminalReason::CapacityLimited) => "capacity limited",
                            Some(TerminalReason::AuthError) => {
                                "auth failed (likely refresh-token reuse)"
                            }
                            _ => "rate limited",
                        };
                        tracing::info!(
                            mission_id = %mission_id,
                            attempt = attempt_idx,
                            reason,
                            "Codex account constrained; leasing next account"
                        );
                        last_constrained_result = Some(result);
                    }
                    _ => {
                        // When rotation tried multiple accounts and every one
                        // was usage-capped, replace the last account's raw
                        // message with an aggregate that makes the rotation
                        // visible and names the soonest reset.
                        let exhausted_all_on_usage_caps = matches!(
                            result.terminal_reason,
                            Some(TerminalReason::RateLimited | TerminalReason::CapacityLimited)
                        ) && usage_capped_outputs.len() >= 2
                            && usage_capped_outputs.len() == attempt_idx;
                        if exhausted_all_on_usage_caps {
                            result.output = summarize_codex_usage_caps(
                                &usage_capped_outputs,
                                usage_capped_outputs.len(),
                            );
                        }
                        break result;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_codex_turn(
    workspace: &Workspace,
    mission_work_dir: &std::path::Path,
    user_message: &str,
    model: Option<&str>,
    model_effort: Option<&str>,
    agent: Option<&str>,
    mission_id: Uuid,
    events_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    app_working_dir: &std::path::Path,
    _session_id: Option<&str>,
    override_credential: Option<&crate::api::ai_providers::CodexCredentialOverride<'_>>,
) -> AgentResult {
    use crate::backend::codex::CodexBackend;
    use crate::backend::events::ExecutionEvent;
    use crate::backend::{Backend, SessionConfig};

    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let model_effort = model_effort.map(str::trim).filter(|m| !m.is_empty());
    let resolved_model: Option<String> = model.map(|m| m.to_string());

    tracing::info!(
        mission_id = %mission_id,
        requested_model = ?model,
        resolved_model = ?resolved_model,
        model_effort = ?model_effort,
        agent = ?agent,
        "Starting Codex turn"
    );

    // Best-effort: try to mint an OpenAI API key from the OAuth refresh token.
    // If this fails (e.g. no API platform org), write_codex_credentials_for_workspace
    // will fall back to auth_mode: "chatgpt" using the access_token directly.
    //
    // Skip this when the rotation layer has already selected a specific
    // ChatGPT OAuth account. Minting an API key refreshes/rotates the same
    // refresh token, then the selected credential can become stale before it
    // is written into Codex auth.json.
    let should_try_mint_api_key = !matches!(
        override_credential,
        Some(crate::api::ai_providers::CodexCredentialOverride::OAuth(_))
    );
    if should_try_mint_api_key {
        if let Err(e) =
            crate::api::ai_providers::ensure_openai_api_key_for_codex(app_working_dir).await
        {
            tracing::warn!(
                "Could not ensure OpenAI API key for Codex (will try chatgpt auth mode): {}",
                e
            );
        }
    }

    let oauth_account_to_prepare = match override_credential {
        Some(crate::api::ai_providers::CodexCredentialOverride::OAuth(account)) => {
            Some((*account).clone())
        }
        Some(crate::api::ai_providers::CodexCredentialOverride::ApiKey(_)) => None,
        None => {
            if crate::api::ai_providers::get_openai_api_key_for_codex_default(app_working_dir)
                .is_none()
            {
                crate::api::ai_providers::get_all_openai_oauth_accounts(app_working_dir)
                    .into_iter()
                    .next()
            } else {
                None
            }
        }
    };
    let prepared_oauth_account = match oauth_account_to_prepare.as_ref() {
        Some(account) => {
            match crate::api::ai_providers::prepare_codex_oauth_account_for_launch(
                app_working_dir,
                account,
            )
            .await
            {
                Ok(account) => Some(account),
                Err(e) => {
                    tracing::error!("Failed to prepare Codex OAuth credentials: {}", e);
                    return AgentResult::failure(
                        format!("Failed to prepare Codex OAuth credentials: {}", e),
                        0,
                    )
                    .with_terminal_reason(TerminalReason::AuthError);
                }
            }
        }
        None => None,
    };
    let prepared_override = prepared_oauth_account
        .as_ref()
        .map(crate::api::ai_providers::CodexCredentialOverride::OAuth);
    let workspace_override = prepared_override.as_ref().or(override_credential);

    // Ensure Codex auth.json is present in the workspace context (host or container).
    if let Err(e) = crate::api::ai_providers::write_codex_credentials_for_workspace(
        workspace,
        app_working_dir,
        workspace_override,
    ) {
        tracing::error!("Failed to write Codex credentials: {}", e);
        return AgentResult::failure(
            format!("Failed to configure Codex authentication: {}", e),
            0,
        )
        .with_terminal_reason(TerminalReason::LlmError);
    }

    let workspace_exec = WorkspaceExec::new(workspace.clone());
    let cli_path = get_backend_string_setting("codex", "cli_path")
        .or_else(|| std::env::var("CODEX_CLI_PATH").ok())
        .unwrap_or_else(|| "codex".to_string());
    let cli_path = match ensure_codex_cli_available(&workspace_exec, mission_work_dir, &cli_path)
        .await
    {
        Ok(path) => path,
        Err(err_msg) => {
            tracing::error!("{}", err_msg);
            return AgentResult::failure(err_msg, 0).with_terminal_reason(TerminalReason::LlmError);
        }
    };

    tracing::info!(
        mission_id = %mission_id,
        workspace_type = ?workspace.workspace_type,
        cli_path = %cli_path,
        model = ?model,
        "Starting Codex execution via WorkspaceExec"
    );

    // DGX Spark build offload (opt-in per workspace) — see
    // Workspace::spark_offload_env. Exported to the codex app-server process so
    // the in-workspace `spark-build` wrapper can reach the host offload endpoint.
    let extra_env = workspace.spark_offload_env(mission_id).unwrap_or_default();

    let codex_config = crate::backend::codex::client::CodexConfig {
        cli_path,
        model_effort: model_effort.map(|s| s.to_string()),
        cancel_token: Some(cancel.clone()),
        extra_env,
        external_chatgpt_auth: prepared_oauth_account.as_ref().map(|account| {
            crate::backend::codex::client::CodexExternalChatgptAuth {
                access_token: account.access_token.clone(),
                chatgpt_account_id: account.chatgpt_account_id.clone(),
                chatgpt_plan_type: None,
                working_dir: app_working_dir.to_path_buf(),
            }
        }),
        ..Default::default()
    };

    // Create Codex backend
    let backend = CodexBackend::with_config_and_workspace(codex_config, workspace_exec);

    // Create session
    let session = match backend
        .create_session(SessionConfig {
            directory: mission_work_dir.to_string_lossy().to_string(),
            title: Some(format!("Mission {}", mission_id)),
            model: resolved_model.clone(),
            agent: agent.map(|s| s.to_string()),
        })
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to create Codex session: {}", e);
            return AgentResult::failure(format!("Failed to start Codex: {}", e), 0)
                .with_terminal_reason(TerminalReason::LlmError);
        }
    };

    // Send message streaming. Codex has no mid-turn injection: the non-goal
    // driver ends the mission on the first `turn/completed`, so an injected
    // `turn/start` would be abandoned. Steers fall back to the authoritative
    // next-turn path (see effective_mid_turn_kind).
    let (mut event_rx, _handle) = match backend.send_message_streaming(&session, user_message).await
    {
        Ok(result) => result,
        Err(e) => {
            let message = format!("Codex execution failed: {}", e);
            tracing::error!("Failed to send message to Codex: {}", e);
            let reason = if is_capacity_limited_error(&message) {
                TerminalReason::CapacityLimited
            } else if is_rate_limited_error(&message) {
                TerminalReason::RateLimited
            } else {
                TerminalReason::LlmError
            };
            return AgentResult::failure(message, 0).with_terminal_reason(reason);
        }
    };

    // Process events until completion or cancellation
    let mut assistant_message = String::new();
    let mut text_delta_coalescer = TextDeltaCoalescer::new();
    let mut text_delta_pending = false;
    let mut success = false;
    let mut error_message: Option<String> = None;
    let mut pending_tools: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut thinking_emitted = false;
    let mut thinking_done_emitted = false;
    let mut thinking_accumulated = String::new();
    // Tracks which codex reasoning item `thinking_accumulated` currently
    // belongs to. When a Thinking event arrives with a different `item_id`,
    // we finalize the existing buffer and start a fresh one — codex emits
    // multiple reasoning items per turn (each with its own cumulative
    // snapshots), and merging them into one buffer produced concatenated
    // thoughts in stored history (see mission dbc8a7e9 seq 6651).
    let mut thinking_item: Option<String> = None;
    let mut last_summary: Option<String> = None;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut tool_events_seen: usize = 0;
    // Set when the cancellation token fires mid-turn. Instead of returning a
    // synthetic "Mission cancelled" failure and discarding everything the
    // model already produced (the common shape for /goal missions, where the
    // closing audit lives in `thinking_accumulated`), we break out of the
    // loop and let the post-loop finalization recover whatever it can.
    let mut cancelled = false;
    let mut codex_goal_cancel_deferred = false;
    let is_goal_request = codex_is_goal_request(user_message);

    loop {
        tokio::select! {
            _ = cancel.cancelled(), if !codex_goal_cancel_deferred => {
                tracing::info!("Codex turn cancelled for mission {}", mission_id);
                if is_goal_request && !codex_goal_cancel_deferred {
                    // Goal-mode cancellation must be handled by the app-server
                    // task because it owns the live thread id needed for
                    // `thread/goal/clear`. Keep draining events until it emits
                    // `ExecutionEvent::Cancelled` (or the channel closes).
                    codex_goal_cancel_deferred = true;
                    continue;
                }
                // Note: Codex process will be cleaned up automatically when
                // the event stream task ends.
                cancelled = true;
                break;
            }
            Some(event) = event_rx.recv() => {
                match event {
                    ExecutionEvent::TextDelta { content } => {
                        // For Codex backend, TextDelta is handled as the latest snapshot for
                        // the currently active assistant message item. Replacing here avoids
                        // concatenating intermediate assistant updates into the final message.
                        assistant_message = content;
                        // P3-#21: rate-limit to ≤1 emit per ~50ms. Skipped
                        // deltas are not lost because the buffer is
                        // cumulative — the next emit replaces it.
                        if text_delta_coalescer.should_emit() {
                            text_delta_pending = false;
                            let _ = events_tx.send(AgentEvent::TextDelta {
                                content: assistant_message.clone(),
                                mission_id: Some(mission_id),
                            });
                        } else {
                            text_delta_pending = true;
                        }
                    }
                    ExecutionEvent::Thinking { content, item_id } => {
                        if thinking_overlaps_visible_answer(&content, &assistant_message) {
                            tracing::debug!(
                                thinking_len = content.len(),
                                assistant_len = assistant_message.len(),
                                "Dropping Codex thinking event that duplicates visible assistant text"
                            );
                            continue;
                        }
                        // Codex emits per-item cumulative snapshots: every
                        // emit with the same `item_id` contains the previous
                        // emit as a prefix. When `item_id` changes we're on a
                        // new reasoning item — finalize the existing buffer
                        // as `done: true` so it persists as its own thought,
                        // and start fresh. Falling back to `merge_stream_fragment`
                        // (the pre-fix behaviour) concatenated unrelated items
                        // into one buffer because it only knows about byte
                        // overlap, not item identity.
                        let item_changed = match (&thinking_item, &item_id) {
                            (Some(prev), Some(cur)) => prev != cur,
                            // First event of the turn, or backend doesn't
                            // expose item IDs: treat as continuation.
                            _ => false,
                        };
                        if item_changed && !thinking_accumulated.is_empty() {
                            let _ = events_tx.send(thinking_final_event(
                                std::mem::take(&mut thinking_accumulated),
                                mission_id,
                            ));
                            thinking_done_emitted = true;
                        }
                        if item_id.is_some() {
                            thinking_item = item_id;
                            // Per-item cumulative: each new snapshot replaces
                            // the buffer (longest wins; shorter echoes are
                            // dropped to keep the buffer monotone).
                            if content.len() >= thinking_accumulated.len() {
                                thinking_accumulated = content;
                            }
                        } else {
                            // Unknown-item backends still use overlap-based
                            // merging so a CLI that resends a partial
                            // snapshot doesn't double words.
                            merge_stream_fragment(&mut thinking_accumulated, &content);
                        }
                        let _ = events_tx.send(AgentEvent::Thinking {
                            content: thinking_accumulated.clone(),
                            done: false,
                            mission_id: Some(mission_id),
                        });
                        if !thinking_accumulated.is_empty() {
                            thinking_done_emitted = false;
                        }
                        thinking_emitted = true;
                    }
                    ExecutionEvent::ToolCall { id, name, args } => {
                        tool_events_seen = tool_events_seen.saturating_add(1);
                        // Flush accumulated thinking as done before tool call,
                        // so the event logger persists the full thought block.
                        if !thinking_accumulated.is_empty() {
                            let _ = events_tx.send(thinking_final_event(
                                std::mem::take(&mut thinking_accumulated),
                                mission_id,
                            ));
                            thinking_done_emitted = true;
                        }
                        thinking_item = None;
                        pending_tools.insert(id.clone(), name.clone());
                        let _ = events_tx.send(AgentEvent::ToolCall {
                            tool_call_id: id,
                            name,
                            args,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::ToolResult { id, name, result } => {
                        tool_events_seen = tool_events_seen.saturating_add(1);
                        pending_tools.remove(&id);
                        let _ = events_tx.send(AgentEvent::ToolResult {
                            tool_call_id: id,
                            name,
                            result,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::TurnSummary { content } => {
                        if !content.trim().is_empty() {
                            last_summary = Some(content);
                        }
                    }
                    ExecutionEvent::Usage { input_tokens, output_tokens } => {
                        total_input_tokens = total_input_tokens.saturating_add(input_tokens);
                        total_output_tokens = total_output_tokens.saturating_add(output_tokens);
                    }
                    ExecutionEvent::GoalIteration { iteration, objective } => {
                        let _ = events_tx.send(AgentEvent::GoalIteration {
                            iteration,
                            objective,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::GoalStatus { status, objective } => {
                        let _ = events_tx.send(AgentEvent::GoalStatus {
                            status,
                            objective,
                            mission_id: Some(mission_id),
                        });
                    }
                    ExecutionEvent::Cancelled => {
                        cancelled = true;
                        break;
                    }
                    ExecutionEvent::Error { message } => {
                        // Codex CLI emits two kinds of post-response errors we
                        // want to treat as non-fatal:
                        //   1. Internal hiccups like "Failed to shutdown rollout
                        //      recorder" that fire after a clean turn.
                        //   2. OpenAI backend returning a 500 mid-stream after
                        //      real content has already been produced; Codex
                        //      retries 5× and then exits with status 1. Our
                        //      client wraps that in "Codex CLI exited before
                        //      completing the turn (exit_status: exit status:
                        //      1). Stderr: <empty> | Stdout: <empty>". The
                        //      earlier assistant_message already captured the
                        //      real response, so the exit error is a downstream
                        //      consequence of the in-stream disconnect we
                        //      already decided to swallow.
                        //
                        // Rule: if we have assistant output and no pending
                        // tools, ignore the error. The empty-output branch
                        // still surfaces startup / auth / config failures
                        // (which produce no text at all). If a tool call is
                        // still pending, the assistant's text is only a
                        // progress update; swallowing a provider error would
                        // mark unfinished work as completed.
                        //
                        // When we do surface an error, prefer the *first*
                        // meaningful message we saw — Codex CLI usually emits
                        // a specific TurnFailed (e.g. "You've hit your usage
                        // limit. ... try again at Apr 28th, 2026 10:03 PM")
                        // before its outer wrapper "Codex CLI exited before
                        // completing the turn (exit_status: exit status: 1).
                        // Stderr: <empty> | Stdout: <empty>". The wrapper is
                        // a generic post-mortem that hides the real cause;
                        // overwriting the specific message with the wrapper
                        // forces the user (and our `is_*_error` classifiers)
                        // to debug from log lines instead of the surfaced
                        // assistant_message.
                        if let Some(surfaced_message) =
                            codex_error_message_to_surface(&assistant_message, &pending_tools, &message)
                        {
                            let recorded = record_codex_error_message(
                                &mut error_message,
                                surfaced_message.clone(),
                            );
                            if recorded {
                                if pending_tools.is_empty() {
                                    tracing::error!("Codex error: {}", surfaced_message);
                                } else {
                                    tracing::warn!(
                                        pending_tool_count = pending_tools.len(),
                                        "Treating post-response Codex error as fatal because tool calls are still pending: {}",
                                        surfaced_message
                                    );
                                }
                            } else {
                                tracing::warn!(
                                    "Keeping prior specific Codex error over generic exit wrapper: existing={}, ignored={}",
                                    error_message.as_deref().unwrap_or(""),
                                    message
                                );
                            }
                        } else {
                            tracing::warn!(
                                "Ignoring post-response Codex error (have {}B assistant output): {}",
                                assistant_message.len(),
                                message
                            );
                        }
                    }
                    ExecutionEvent::MessageComplete { session_id: _ } => {
                        success = error_message.is_none();
                        break;
                    }
                }
            }
            else => {
                // Channel closed
                break;
            }
        }
    }

    // P3-#21 final flush: ensure the closing delta the coalescer may
    // have suppressed reaches the dashboard. AssistantMessage emits
    // below will replace it, so this is purely a safety net for clients
    // that render the streaming buffer ahead of completion.
    if text_delta_pending {
        let _ = events_tx.send(AgentEvent::TextDelta {
            content: assistant_message.clone(),
            mission_id: Some(mission_id),
        });
    }

    // Capture a copy of the accumulated reasoning before the flush below
    // moves it into the broadcast event. /goal missions frequently end with
    // the model emitting a self-audit as reasoning and then calling
    // `update_goal { status: "complete" }` without a closing chat message;
    // in that case `assistant_message` is empty (or stale from an earlier
    // iteration) and the only place the audit lives is `thinking_accumulated`.
    let thinking_for_fallback = if thinking_accumulated.trim().is_empty() {
        None
    } else {
        Some(thinking_accumulated.clone())
    };

    // Flush any remaining accumulated thinking with full content so
    // the event logger persists it for replay/history.
    if thinking_emitted && !thinking_done_emitted {
        let _ = events_tx.send(thinking_final_event(thinking_accumulated, mission_id));
    }

    let no_output = assistant_message.trim().is_empty()
        && last_summary.is_none()
        && thinking_for_fallback.is_none();
    if no_output && error_message.is_none() && !cancelled {
        success = false;
        error_message = Some(
            "Codex produced no output. This usually means the Codex CLI failed before emitting JSON (often authentication). Check that the host has a valid `~/.codex/auth.json` and that the backend can access it."
                .to_string(),
        );
    }

    // Snapshot the cancel marker (output + terminal_reason) once. The marker
    // reads `is_shutdown_initiated()` internally, and a shutdown signal
    // arriving between two reads could pair "Mission cancelled" text with a
    // ServerShutdown reason (or vice versa) — TOCTOU race flagged by bugbot.
    let cancel_marker = if cancelled {
        Some(cancel_or_shutdown_failure())
    } else {
        None
    };

    let mut final_message = if let Some(err) = error_message {
        err
    } else if !assistant_message.is_empty() {
        assistant_message
    } else if let Some(summary) = last_summary {
        summary
    } else if let Some(thinking_text) = thinking_for_fallback {
        if success && codex_is_goal_request(user_message) && !cancelled {
            codex_missing_goal_final_response_message()
        } else {
            // Surface the model's reasoning as the assistant message so the
            // dashboard's final-message slot matches what's already visible in
            // the thinking panel.
            thinking_text
        }
    } else if let Some(marker) = cancel_marker.as_ref() {
        // Mid-turn cancellation with nothing accumulated — preserve the
        // historical "Mission cancelled" / shutdown text for the UI.
        marker.output.clone()
    } else {
        "No response from Codex".to_string()
    };

    let tool_activity_required = codex_turn_requires_tool_activity(user_message, &final_message);
    let stopped_before_required_tools = success && tool_events_seen == 0 && tool_activity_required;
    let stopped_on_progress_update = success
        && tool_activity_required
        && codex_final_message_looks_like_progress_update(&final_message);
    let stopped_with_pending_tool_error =
        !success && final_message.starts_with(CODEX_PENDING_TOOLS_ERROR_PREFIX);
    if stopped_before_required_tools || stopped_on_progress_update {
        tracing::warn!(
            mission_id = %mission_id,
            output_len = final_message.len(),
            tool_events_seen = tool_events_seen,
            stopped_on_progress_update = stopped_on_progress_update,
            "Codex turn completed before satisfying a tool-required prompt"
        );
        success = false;
        final_message = format!(
            "Codex stopped before completing required workspace/tool steps. Last response:\n\n{}",
            final_message.trim()
        );
    }

    let lower_final = final_message.to_lowercase();
    if lower_final.contains("does not exist or you do not have access")
        || lower_final.contains("model_not_found")
    {
        final_message.push_str("\n\nTry model `gpt-5.5` or `gpt-5-codex` for Codex missions.");
        if matches!(
            model,
            Some("gpt-5.3-codex" | "gpt-5.4-codex" | "gpt-5.5-codex")
        ) {
            final_message.push_str(
                "\n\nIf you expected this Codex model to work, your Codex CLI may be outdated. \
Update it to the latest version (`npm install -g @openai/codex@latest`) and retry.",
            );
        }
    }

    let usage = crate::cost::TokenUsage {
        input_tokens: total_input_tokens,
        output_tokens: total_output_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };

    let model_for_cost = resolved_model.as_deref();
    let (cost_cents, cost_source) = resolve_cost_cents_and_source(None, model_for_cost, &usage);

    let mut result = if let Some(marker) = cancel_marker {
        // Cancellation outranks success/error classification: keep the partial
        // assistant_message / thinking content as the visible final message
        // but mark the mission Interrupted (or ServerShutdown) so the
        // dashboard renders the resume affordance and not a fake completion.
        // Reusing the marker from the final-message picker keeps the
        // text/reason pair consistent if shutdown fires mid-finalize.
        let cancel_reason = marker.terminal_reason.unwrap_or(TerminalReason::Cancelled);
        AgentResult::failure(final_message, cost_cents).with_terminal_reason(cancel_reason)
    } else if success {
        AgentResult::success(final_message, cost_cents)
            .with_terminal_reason(TerminalReason::TurnComplete)
    } else {
        // Distinguish provider concurrency exhaustion from classic rate limits.
        // Refresh-token reuse (ChatGPT OAuth races between sibling missions)
        // is_auth_error-classified so the codex arm rotates to another
        // configured account instead of surfacing the bare error.
        let reason = if stopped_before_required_tools || stopped_on_progress_update {
            TerminalReason::Stalled
        } else if is_capacity_limited_error(&final_message) {
            TerminalReason::CapacityLimited
        } else if is_rate_limited_error(&final_message) {
            TerminalReason::RateLimited
        } else if is_auth_error(&final_message) {
            TerminalReason::AuthError
        } else if stopped_with_pending_tool_error {
            TerminalReason::Stalled
        } else {
            TerminalReason::LlmError
        };
        AgentResult::failure(final_message, cost_cents).with_terminal_reason(reason)
    };

    let outcome = turn_outcome_for_result(
        &result,
        CompletionSignal::NativeTerminal,
        CompletionConfidence::High,
    );
    result = result.with_turn_outcome(outcome);
    result = result.with_cost_source(cost_source);
    if usage.has_usage() {
        result = result.with_usage(usage);
    }
    if let Some(m) = resolved_model.as_deref() {
        result = result.with_model(m.to_string());
    }

    result
}
