use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if let Err(err) = run(env::args().skip(1).collect()) {
        eprintln!("palomactl: {err:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let root = env::var("PALOMACTL_ROOT")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    let Some(command) = args.first().map(String::as_str) else {
        print_usage();
        return Ok(());
    };

    match command {
        "status" => {
            let state = load_state(&root)?;
            println!("{}", render_status_text(&state));
        }
        "reconcile" => {
            let state = reconcile(&root)?;
            println!(
                "wrote {}\nwrote {}\ndrift={}",
                control_dir(&root).join("status.json").display(),
                control_dir(&root).join("status.md").display(),
                state.drift.len()
            );
        }
        "pr-gates" => {
            let repo = args.get(1).ok_or_else(|| anyhow!("missing owner/repo"))?;
            let number = args
                .get(2)
                .ok_or_else(|| anyhow!("missing PR number"))?
                .parse::<u64>()
                .context("PR number must be numeric")?;
            let gates = pr_gates(&root, repo, number)?;
            println!("{}", serde_json::to_string_pretty(&gates)?);
            if let Some(rec) = gates.recommendation.as_deref() {
                println!("recommendation={rec}");
            }
        }
        "set-mode" => {
            let project = args.get(1).ok_or_else(|| anyhow!("missing project"))?;
            let mode = args.get(2).ok_or_else(|| anyhow!("missing mode"))?;
            let until_pos = args
                .iter()
                .position(|a| a == "--until")
                .ok_or_else(|| anyhow!("missing --until <iso>"))?;
            let until = args
                .get(until_pos + 1)
                .ok_or_else(|| anyhow!("missing --until value"))?;
            let policy = set_mode(&root, project, mode, until)?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        "dispatch-plan" => {
            let project = value_after_flag(&args, "--project")
                .ok_or_else(|| anyhow!("missing --project <slug>"))?;
            let plan = dispatch_plan(&root, project)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        _ => {
            print_usage();
            bail!("unknown command {command}");
        }
    }
    Ok(())
}

fn print_usage() {
    println!(
        "usage:\n  palomactl status\n  palomactl reconcile\n  palomactl pr-gates <owner/repo> <number>\n  palomactl set-mode <project> <mode> --until <iso>\n  palomactl dispatch-plan --project <slug>"
    );
}

fn value_after_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn control_dir(root: &Path) -> PathBuf {
    root.join(".paloma")
}

fn fixtures_dir(root: &Path) -> PathBuf {
    control_dir(root).join("fixtures")
}

fn modes_dir(root: &Path) -> PathBuf {
    control_dir(root).join("modes")
}

fn validate_project_slug(project: &str) -> Result<()> {
    if project.is_empty()
        || project == "."
        || project == ".."
        || project.contains("..")
        || project.contains('/')
        || project.contains('\\')
        || !project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        bail!("project slug must be non-empty and contain only ASCII letters, numbers, '-' or '_'");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MissionSnapshot {
    #[serde(default)]
    missions: Vec<MissionRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MissionRef {
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    track: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    github_pr: Option<String>,
    #[serde(default)]
    desired_state: Option<String>,
    #[serde(default)]
    parent_mission_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DriftKind {
    ShortMissionId,
    StaleMissionRef,
    PrListedOpenButMerged,
    LiveMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Drift {
    kind: DriftKind,
    source: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectDoc {
    path: String,
    mission_refs: Vec<String>,
    pr_refs: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProjectState {
    generated_at: String,
    project_docs: Vec<ProjectDoc>,
    mission_counts: BTreeMap<String, usize>,
    missions: Vec<MissionRef>,
    drift: Vec<Drift>,
}

fn load_state(root: &Path) -> Result<ProjectState> {
    let missions = load_missions(root)?;
    let docs = scan_project_docs(root)?;
    let drift = detect_drift(root, &docs, &missions)?;
    let mut counts = BTreeMap::new();
    for mission in &missions {
        *counts
            .entry(if mission.status.is_empty() {
                "unknown".to_string()
            } else {
                mission.status.clone()
            })
            .or_insert(0) += 1;
    }
    Ok(ProjectState {
        generated_at: Utc::now().to_rfc3339(),
        project_docs: docs,
        mission_counts: counts,
        missions,
        drift,
    })
}

fn reconcile(root: &Path) -> Result<ProjectState> {
    let state = load_state(root)?;
    fs::create_dir_all(control_dir(root))?;
    fs::write(
        control_dir(root).join("status.json"),
        serde_json::to_vec_pretty(&state)?,
    )?;
    fs::write(
        control_dir(root).join("status.md"),
        render_status_md(&state),
    )?;
    Ok(state)
}

fn load_missions(root: &Path) -> Result<Vec<MissionRef>> {
    let path = fixtures_dir(root).join("missions.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    if value.is_array() {
        Ok(serde_json::from_value(value)?)
    } else {
        let snapshot: MissionSnapshot = serde_json::from_value(value)?;
        Ok(snapshot.missions)
    }
}

fn scan_project_docs(root: &Path) -> Result<Vec<ProjectDoc>> {
    let mut paths = Vec::new();
    for dir in [control_dir(root).join("projects"), root.join("projects")] {
        if dir.exists() {
            collect_md(&dir, &mut paths)?;
        }
    }
    if paths.is_empty() {
        let docs = root.join("docs");
        if docs.exists() {
            for entry in fs::read_dir(docs)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md")
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("PALOMA_"))
                        .unwrap_or(false)
                {
                    paths.push(path);
                }
            }
        }
    }

    let mut out = Vec::new();
    let mission_re = Regex::new(r"(?i)\bmission(?:[_ -]?id)?[:# ]+([0-9a-f][0-9a-f-]{5,36})\b")?;
    let pr_re = Regex::new(r"(?i)\b(?:pr|pull request)\s*#?(\d+)\b")?;
    for path in paths {
        let text = fs::read_to_string(&path)?;
        let mission_refs = mission_re
            .captures_iter(&text)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        let pr_refs = pr_re
            .captures_iter(&text)
            .filter_map(|c| c.get(1)?.as_str().parse().ok())
            .collect();
        out.push(ProjectDoc {
            path: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            mission_refs,
            pr_refs,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn collect_md(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_md(&path, paths)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn detect_drift(root: &Path, docs: &[ProjectDoc], missions: &[MissionRef]) -> Result<Vec<Drift>> {
    let mut drift = Vec::new();
    let live_ids: BTreeSet<&str> = missions.iter().map(|m| m.id.as_str()).collect();
    for doc in docs {
        let text = fs::read_to_string(root.join(&doc.path)).unwrap_or_default();
        for mission_id in &doc.mission_refs {
            if !is_uuid_like(mission_id) {
                drift.push(Drift {
                    kind: DriftKind::ShortMissionId,
                    source: doc.path.clone(),
                    detail: format!("mission ref `{mission_id}` is not a full UUID"),
                });
            } else if !live_ids.contains(mission_id.as_str()) {
                drift.push(Drift {
                    kind: DriftKind::StaleMissionRef,
                    source: doc.path.clone(),
                    detail: format!("mission `{mission_id}` is not in live missions"),
                });
            }
        }

        for mission in missions {
            if text.contains(&mission.id) {
                let status_re = Regex::new(&format!(
                    r"(?i){}\b.*\bstatus[:= ]+([a-z_]+)",
                    regex::escape(&mission.id)
                ))?;
                if let Some(found) = status_re
                    .captures(&text)
                    .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                {
                    if !mission.status.is_empty() && found != mission.status {
                        drift.push(Drift {
                            kind: DriftKind::LiveMismatch,
                            source: doc.path.clone(),
                            detail: format!(
                                "mission `{}` tracker status `{}` differs from live `{}`",
                                mission.id, found, mission.status
                            ),
                        });
                    }
                }
            }
        }

        for pr in &doc.pr_refs {
            if line_mentions_open_pr(&text, *pr) {
                if let Some(gates) = load_pr_fixture(root, "fixture/repo", *pr) {
                    if gates.merged {
                        drift.push(Drift {
                            kind: DriftKind::PrListedOpenButMerged,
                            source: doc.path.clone(),
                            detail: format!("PR #{pr} is listed open but live PR is merged"),
                        });
                    }
                }
            }
        }
    }
    Ok(drift)
}

fn is_uuid_like(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(parts.iter())
            .all(|(len, part)| part.len() == *len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn line_mentions_open_pr(text: &str, number: u64) -> bool {
    let pr_re = Regex::new(&format!(r"(?i)(?:\bpr\s*#?\s*{}\b|#{}\b)", number, number))
        .expect("valid PR mention regex");
    let open_re = Regex::new(r"(?i)\bopen\b").expect("valid open status regex");
    text.lines()
        .any(|line| pr_re.is_match(line) && open_re.is_match(line))
}

fn render_status_text(state: &ProjectState) -> String {
    let mut out = String::new();
    out.push_str("paloma control-plane status\n");
    out.push_str(&format!("project_docs={}\n", state.project_docs.len()));
    out.push_str(&format!("missions={}\n", state.missions.len()));
    for (status, count) in &state.mission_counts {
        out.push_str(&format!("mission_status.{status}={count}\n"));
    }
    out.push_str(&format!("drift={}\n", state.drift.len()));
    for drift in &state.drift {
        out.push_str(&format!(
            "- {:?}: {} ({})\n",
            drift.kind, drift.detail, drift.source
        ));
    }
    out
}

fn render_status_md(state: &ProjectState) -> String {
    let mut out = String::new();
    out.push_str("# Paloma Status\n\n");
    out.push_str(&format!("Generated: `{}`\n\n", state.generated_at));
    out.push_str("## Mission Counts\n\n");
    for (status, count) in &state.mission_counts {
        out.push_str(&format!("- `{status}`: {count}\n"));
    }
    if state.mission_counts.is_empty() {
        out.push_str("- none\n");
    }
    out.push_str("\n## Drift\n\n");
    for drift in &state.drift {
        out.push_str(&format!(
            "- `{:?}`: {} ({})\n",
            drift.kind, drift.detail, drift.source
        ));
    }
    if state.drift.is_empty() {
        out.push_str("- none\n");
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CheckRun {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PrGates {
    #[serde(default)]
    owner_repo: String,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    mergeable: Option<bool>,
    #[serde(default, alias = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default)]
    conflicting: bool,
    #[serde(default)]
    checks: Vec<CheckRun>,
    #[serde(default)]
    bugbot_accessible: bool,
    #[serde(default)]
    recommendation: Option<String>,
}

fn pr_gates(root: &Path, owner_repo: &str, number: u64) -> Result<PrGates> {
    let mut gates = load_pr_fixture(root, owner_repo, number)
        .or_else(|| gh_pr_gates(owner_repo, number))
        .ok_or_else(|| anyhow!("could not read PR gates from fixture or gh"))?;
    gates.owner_repo = owner_repo.to_string();
    gates.number = number;
    gates.conflicting = gates.conflicting
        || matches!(
            gates.merge_state_status.as_deref(),
            Some("dirty" | "blocked" | "behind")
        )
        || gates.mergeable == Some(false);
    let checks_green = !gates.checks.is_empty()
        && gates
            .checks
            .iter()
            .all(|c| c.status == "completed" && c.conclusion.as_deref() == Some("success"));
    let closed = gates.state.eq_ignore_ascii_case("closed");
    gates.recommendation = if gates.draft {
        Some("wait_for_ready_for_review".to_string())
    } else if gates.conflicting {
        Some("needs_conflict_resolution".to_string())
    } else if gates.merged {
        Some("already_merged".to_string())
    } else if closed {
        Some("closed_without_merge".to_string())
    } else if checks_green {
        Some("ready_for_review_or_merge".to_string())
    } else {
        Some("needs_checks_or_review".to_string())
    };
    Ok(gates)
}

fn load_pr_fixture(root: &Path, owner_repo: &str, number: u64) -> Option<PrGates> {
    let safe_repo = owner_repo.replace('/', "__");
    let candidates = [
        fixtures_dir(root).join(format!("pr-{safe_repo}-{number}.json")),
        fixtures_dir(root).join(format!("pr-{number}.json")),
    ];
    candidates.iter().find_map(|path| {
        if path.exists() {
            serde_json::from_slice(&fs::read(path).ok()?).ok()
        } else {
            None
        }
    })
}

fn gh_pr_gates(owner_repo: &str, number: u64) -> Option<PrGates> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            owner_repo,
            "--json",
            "state,isDraft,mergeable,mergeStateStatus,merged,statusCheckRollup,comments",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let checks = value
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| CheckRun {
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    status: item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                    conclusion: item
                        .get("conclusion")
                        .and_then(Value::as_str)
                        .map(|s| s.to_ascii_lowercase()),
                })
                .collect()
        })
        .unwrap_or_default();
    let bugbot_accessible = value
        .get("comments")
        .and_then(Value::as_array)
        .map(|comments| {
            comments.iter().any(|comment| {
                comment
                    .get("author")
                    .and_then(|a| a.get("login"))
                    .and_then(Value::as_str)
                    .map(|login| login.to_ascii_lowercase().contains("bugbot"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    Some(PrGates {
        owner_repo: owner_repo.to_string(),
        number,
        state: value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase(),
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        merged: value
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        mergeable: value
            .get("mergeable")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "MERGEABLE" => Some(true),
                "CONFLICTING" => Some(false),
                _ => None,
            }),
        merge_state_status: value
            .get("mergeStateStatus")
            .and_then(Value::as_str)
            .map(|s| s.to_ascii_lowercase()),
        conflicting: false,
        checks,
        bugbot_accessible,
        recommendation: None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RiskMode {
    Conservative,
    Normal,
    Aggressive,
    VeryAggressive,
}

impl RiskMode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "conservative" => Ok(Self::Conservative),
            "normal" => Ok(Self::Normal),
            "aggressive" => Ok(Self::Aggressive),
            "very_aggressive" => Ok(Self::VeryAggressive),
            _ => bail!("mode must be conservative|normal|aggressive|very_aggressive"),
        }
    }

    fn exploration_limit(&self) -> usize {
        match self {
            Self::Conservative => 0,
            Self::Normal => 1,
            Self::Aggressive => 3,
            Self::VeryAggressive => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModePolicy {
    project: String,
    mode: RiskMode,
    until: String,
    safe_lane_max_workers: usize,
    exploration_lane_max_workers: usize,
}

fn normal_mode_policy(project: &str) -> ModePolicy {
    ModePolicy {
        project: project.to_string(),
        mode: RiskMode::Normal,
        until: "unset".to_string(),
        safe_lane_max_workers: 1,
        exploration_lane_max_workers: RiskMode::Normal.exploration_limit(),
    }
}

fn parse_unexpired_until(until: &str) -> Result<DateTime<Utc>> {
    let until = DateTime::parse_from_rfc3339(until)
        .context("--until must be RFC3339/ISO timestamp")?
        .with_timezone(&Utc);
    if until <= Utc::now() {
        bail!("--until must be in the future");
    }
    Ok(until)
}

fn set_mode(root: &Path, project: &str, mode: &str, until: &str) -> Result<ModePolicy> {
    validate_project_slug(project)?;
    parse_unexpired_until(until)?;
    let mode = RiskMode::parse(mode)?;
    let policy = ModePolicy {
        project: project.to_string(),
        safe_lane_max_workers: 1,
        exploration_lane_max_workers: mode.exploration_limit(),
        mode,
        until: until.to_string(),
    };
    fs::create_dir_all(modes_dir(root))?;
    fs::write(
        modes_dir(root).join(format!("{project}.json")),
        serde_json::to_vec_pretty(&policy)?,
    )?;
    Ok(policy)
}

fn load_mode(root: &Path, project: &str) -> Result<ModePolicy> {
    validate_project_slug(project)?;
    let path = modes_dir(root).join(format!("{project}.json"));
    if path.exists() {
        let Some(policy) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ModePolicy>(&bytes).ok())
        else {
            return Ok(normal_mode_policy(project));
        };
        if DateTime::parse_from_rfc3339(&policy.until)
            .map(|until| until.with_timezone(&Utc) > Utc::now())
            .unwrap_or(false)
        {
            Ok(policy)
        } else {
            Ok(normal_mode_policy(project))
        }
    } else {
        Ok(normal_mode_policy(project))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TaskFixture {
    #[serde(default)]
    project: String,
    #[serde(default, alias = "task_key")]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    lane: String,
    #[serde(default)]
    intent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DispatchPlan {
    project: String,
    mode: RiskMode,
    safe_lane_max_workers: usize,
    exploration_lane_max_workers: usize,
    dispatch: Vec<Value>,
    reason: Option<String>,
}

fn dispatch_plan(root: &Path, project: &str) -> Result<DispatchPlan> {
    let mode = load_mode(root, project)?;
    let tasks = load_tasks(root)?;
    let ready: Vec<TaskFixture> = tasks
        .into_iter()
        .filter(|t| t.project == project)
        .filter(|t| matches!(t.status.as_str(), "ready" | "pending"))
        .collect();
    if ready.is_empty() {
        return Ok(DispatchPlan {
            project: project.to_string(),
            mode: mode.mode,
            safe_lane_max_workers: 1,
            exploration_lane_max_workers: mode.exploration_lane_max_workers,
            dispatch: Vec::new(),
            reason: Some("no_ready_tasks".to_string()),
        });
    }

    let mut safe = Vec::new();
    let mut exploration = Vec::new();
    for task in ready {
        if is_exploration_task(&task) {
            exploration.push(task);
        } else {
            safe.push(task);
        }
    }
    safe.sort_by(|a, b| a.id.cmp(&b.id));
    exploration.sort_by(|a, b| a.id.cmp(&b.id));

    let mut dispatch = Vec::new();
    for task in safe.into_iter().take(1) {
        dispatch.push(json!({
            "task_id": task.id,
            "lane": "safe",
            "max_workers": 1,
            "gate_protected": true
        }));
    }
    for task in exploration
        .into_iter()
        .take(mode.exploration_lane_max_workers)
    {
        dispatch.push(json!({
            "task_id": task.id,
            "lane": "exploration",
            "max_workers": mode.exploration_lane_max_workers,
            "isolated_branch": true
        }));
    }

    Ok(DispatchPlan {
        project: project.to_string(),
        mode: mode.mode,
        safe_lane_max_workers: 1,
        exploration_lane_max_workers: mode.exploration_lane_max_workers,
        dispatch,
        reason: None,
    })
}

fn is_exploration_task(task: &TaskFixture) -> bool {
    task.lane == "exploration"
        || task
            .intent
            .as_deref()
            .map(intent_requests_exploration)
            .unwrap_or(false)
}

fn intent_requests_exploration(intent: &str) -> bool {
    intent
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "proof"
                    | "prove"
                    | "explore"
                    | "explores"
                    | "explored"
                    | "exploring"
                    | "exploration"
                    | "exploratory"
            )
        })
}

fn load_tasks(root: &Path) -> Result<Vec<TaskFixture>> {
    let path = fixtures_dir(root).join("tasks.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    if value.is_array() {
        Ok(serde_json::from_value(value)?)
    } else {
        Ok(serde_json::from_value(
            value
                .get("tasks")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn detects_short_mission_ids() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/projects/verity.md"),
            "Tracker: mission abc1234 status=active\n",
        );
        let state = load_state(tmp.path()).unwrap();
        assert!(state
            .drift
            .iter()
            .any(|d| d.kind == DriftKind::ShortMissionId));
    }

    #[test]
    fn detects_stale_tracker_refs() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/fixtures/missions.json"),
            r#"[{"id":"11111111-1111-4111-8111-111111111111","status":"active"}]"#,
        );
        write(
            &tmp.path().join(".paloma/projects/verity.md"),
            "mission: 22222222-2222-4222-8222-222222222222 status=active\n",
        );
        let state = load_state(tmp.path()).unwrap();
        assert!(state
            .drift
            .iter()
            .any(|d| d.kind == DriftKind::StaleMissionRef));
    }

    #[test]
    fn pr_103_green_but_conflicting_recommends_conflict_resolution() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/fixtures/pr-103.json"),
            r#"{
              "state":"open",
              "draft":false,
              "merged":false,
              "mergeable":false,
              "merge_state_status":"dirty",
              "checks":[{"name":"ci","status":"completed","conclusion":"success"}]
            }"#,
        );
        let gates = pr_gates(tmp.path(), "lfglabs-dev/verity", 103).unwrap();
        assert_eq!(
            gates.recommendation.as_deref(),
            Some("needs_conflict_resolution")
        );
    }

    #[test]
    fn closed_unmerged_pr_with_green_checks_is_not_recommended_for_merge() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/fixtures/pr-104.json"),
            r#"{
              "state":"closed",
              "draft":false,
              "merged":false,
              "mergeable":true,
              "checks":[{"name":"ci","status":"completed","conclusion":"success"}]
            }"#,
        );
        let gates = pr_gates(tmp.path(), "lfglabs-dev/verity", 104).unwrap();
        assert_eq!(
            gates.recommendation.as_deref(),
            Some("closed_without_merge")
        );
    }

    #[test]
    fn intent_routing_matches_exploration_tokens_not_substrings() {
        let safe_intents = ["proofread docs", "waterproof deployment notes"];
        for intent in safe_intents {
            let task = TaskFixture {
                lane: "safe".to_string(),
                intent: Some(intent.to_string()),
                ..Default::default()
            };
            assert!(!is_exploration_task(&task), "{intent} should stay safe");
        }

        let exploration_intents = [
            "proof of concept",
            "prove this approach",
            "explore options",
            "exploration spike",
        ];
        for intent in exploration_intents {
            let task = TaskFixture {
                lane: "safe".to_string(),
                intent: Some(intent.to_string()),
                ..Default::default()
            };
            assert!(
                is_exploration_task(&task),
                "{intent} should route exploration"
            );
        }
    }

    #[test]
    fn aggressive_mode_increases_exploration_but_not_safe_lane() {
        let tmp = tempdir().unwrap();
        set_mode(
            tmp.path(),
            "verity-core",
            "aggressive",
            "2999-07-02T00:00:00Z",
        )
        .unwrap();
        write(
            &tmp.path().join(".paloma/fixtures/tasks.json"),
            r#"[
              {"project":"verity-core","id":"fix-pr","status":"ready","lane":"safe"},
              {"project":"verity-core","id":"proof-a","status":"ready","lane":"exploration"},
              {"project":"verity-core","id":"proof-b","status":"ready","lane":"exploration"},
              {"project":"verity-core","id":"proof-c","status":"ready","lane":"exploration"}
            ]"#,
        );
        let plan = dispatch_plan(tmp.path(), "verity-core").unwrap();
        assert_eq!(plan.safe_lane_max_workers, 1);
        assert_eq!(plan.exploration_lane_max_workers, 3);
        assert_eq!(
            plan.dispatch.iter().filter(|d| d["lane"] == "safe").count(),
            1
        );
        assert_eq!(
            plan.dispatch
                .iter()
                .filter(|d| d["lane"] == "exploration")
                .count(),
            3
        );
    }

    #[test]
    fn set_mode_rejects_past_until() {
        let tmp = tempdir().unwrap();
        let err = set_mode(
            tmp.path(),
            "verity-core",
            "aggressive",
            "2000-01-01T00:00:00Z",
        )
        .unwrap_err();
        assert!(err.to_string().contains("--until must be in the future"));
        assert!(!tmp.path().join(".paloma/modes/verity-core.json").exists());
    }

    #[test]
    fn expired_mode_reverts_to_normal_exploration_limit() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/modes/verity-core.json"),
            r#"{
              "project":"verity-core",
              "mode":"very_aggressive",
              "until":"2000-01-01T00:00:00Z",
              "safe_lane_max_workers":1,
              "exploration_lane_max_workers":5
            }"#,
        );
        write(
            &tmp.path().join(".paloma/fixtures/tasks.json"),
            r#"[
              {"project":"verity-core","id":"proof-a","status":"ready","lane":"exploration"},
              {"project":"verity-core","id":"proof-b","status":"ready","lane":"exploration"}
            ]"#,
        );

        let plan = dispatch_plan(tmp.path(), "verity-core").unwrap();
        assert_eq!(plan.mode, RiskMode::Normal);
        assert_eq!(plan.exploration_lane_max_workers, 1);
        assert_eq!(
            plan.dispatch
                .iter()
                .filter(|d| d["lane"] == "exploration")
                .count(),
            1
        );
    }

    #[test]
    fn unparsable_mode_reverts_to_normal_exploration_limit() {
        let tmp = tempdir().unwrap();
        write(&tmp.path().join(".paloma/modes/verity-core.json"), "{ nope");
        write(
            &tmp.path().join(".paloma/fixtures/tasks.json"),
            r#"[
              {"project":"verity-core","id":"proof-a","status":"ready","lane":"exploration"},
              {"project":"verity-core","id":"proof-b","status":"ready","lane":"exploration"}
            ]"#,
        );

        let plan = dispatch_plan(tmp.path(), "verity-core").unwrap();
        assert_eq!(plan.mode, RiskMode::Normal);
        assert_eq!(plan.exploration_lane_max_workers, 1);
    }

    #[test]
    fn invalid_project_slug_cannot_escape_modes_dir() {
        let tmp = tempdir().unwrap();
        let err = set_mode(
            tmp.path(),
            "../status",
            "aggressive",
            "2999-07-02T00:00:00Z",
        )
        .unwrap_err();
        assert!(err.to_string().contains("project slug"));
        assert!(!tmp.path().join(".paloma/status.json").exists());
        assert!(!tmp.path().join(".paloma/modes/../status.json").exists());
    }

    #[test]
    fn detects_open_pr_listed_as_merged_from_fixture_without_live_repo() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/projects/verity.md"),
            "Tracker: PR #103 open\n",
        );
        write(
            &tmp.path().join(".paloma/fixtures/pr-103.json"),
            r#"{
              "state":"closed",
              "draft":false,
              "merged":true,
              "checks":[{"name":"ci","status":"completed","conclusion":"success"}]
            }"#,
        );

        let state = load_state(tmp.path()).unwrap();
        assert!(state
            .drift
            .iter()
            .any(|d| d.kind == DriftKind::PrListedOpenButMerged));
    }

    #[test]
    fn open_pr_matching_rejects_substrings_and_partial_numbers() {
        assert!(line_mentions_open_pr("PR #12 open", 12));
        assert!(line_mentions_open_pr("open pull request mentions #12", 12));
        assert!(!line_mentions_open_pr("PR #12 reopened", 12));
        assert!(!line_mentions_open_pr("PR #12 opening soon", 12));
        assert!(!line_mentions_open_pr("PR #123 open", 12));
        assert!(!line_mentions_open_pr("PR #12 open", 123));
    }

    #[test]
    fn beal_no_ready_tasks_means_no_worker_dispatch() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".paloma/fixtures/tasks.json"),
            r#"[
              {"project":"beal","id":"b1","status":"blocked","lane":"exploration"},
              {"project":"beal","id":"b2","status":"accepted","lane":"safe"}
            ]"#,
        );
        let plan = dispatch_plan(tmp.path(), "beal").unwrap();
        assert!(plan.dispatch.is_empty());
        assert_eq!(plan.reason.as_deref(), Some("no_ready_tasks"));
    }

    #[test]
    fn reconcile_writes_generated_status_without_editing_project_markdown() {
        let tmp = tempdir().unwrap();
        let project = tmp.path().join(".paloma/projects/verity.md");
        write(&project, "mission abc1234\n");
        let before = fs::read_to_string(&project).unwrap();
        reconcile(tmp.path()).unwrap();
        assert!(tmp.path().join(".paloma/status.json").exists());
        assert!(tmp.path().join(".paloma/status.md").exists());
        assert_eq!(before, fs::read_to_string(&project).unwrap());
    }
}
