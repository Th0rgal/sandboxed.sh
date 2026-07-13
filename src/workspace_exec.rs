//! Workspace execution layer.
//!
//! Spawns processes inside a workspace execution context so that:
//! - Host workspaces execute directly on the host
//! - Container workspaces execute via systemd-nspawn in the container filesystem
//!
//! This is used for per-workspace Claude Code and OpenCode execution.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::process::{Child, Command};

use crate::nspawn;
use crate::util::env_var_bool;
use crate::workspace::{use_nspawn_for_workspace, TailscaleMode, Workspace, WorkspaceType};

const CONTAINER_KEEPALIVE_ENV_KEY: &str = "SANDBOXED_SH_CONTAINER_KEEPALIVE";
const CONTAINER_KEEPALIVE_ENV_VALUE: &str = "1";
const ALLOW_TRANSIENT_CONTAINER_NSENTER_ENV: &str =
    "SANDBOXED_SH_ALLOW_TRANSIENT_CONTAINER_NSENTER";
const NSENTER_USE_TARGET_ROOT_ENV: &str = "SANDBOXED_SH_NSENTER_USE_TARGET_ROOT";

fn replace_command_env(cmd: &mut Command, env: HashMap<String, String>) {
    cmd.env_clear().envs(env);
}

/// TLS CA-bundle env vars that point at a filesystem path. When the host
/// process that launches a container mission (e.g. the sandboxed.sh daemon
/// started from a Python venv) has one of these set, the value leaks into the
/// container via process inheritance on the `nsenter` path — but the path
/// (e.g. a Hermes venv `certifi/cacert.pem`) does not exist inside the
/// container rootfs. OpenSSL/curl/Node then fail to load *any* CA store and
/// every HTTPS request dies, which the provider preflight reports as a
/// misleading DNS/connectivity failure. See `ca_env_scrub_prelude`.
const CA_BUNDLE_ENV_VARS: &[&str] = &[
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
];

const CONTAINER_DEFAULT_PATH_DIRS: &[&str] = &[
    "/root/.bun/bin",
    "/root/.cache/.bun/bin",
    "/root/.local/bin",
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
];

fn normalize_container_path(existing: Option<&str>) -> String {
    let mut dirs = Vec::new();

    if let Some(existing) = existing {
        for dir in existing.split(':') {
            let dir = dir.trim();
            if dir.is_empty() || dirs.iter().any(|existing| existing == dir) {
                continue;
            }
            dirs.push(dir.to_string());
        }
    }

    for dir in CONTAINER_DEFAULT_PATH_DIRS {
        if !dirs.iter().any(|existing| existing == dir) {
            dirs.push((*dir).to_string());
        }
    }

    dirs.join(":")
}

/// POSIX-sh snippet that scrubs inherited-but-broken TLS CA-bundle env vars.
///
/// This is prepended to the `/bin/sh -lc` command that `nsenter` runs *inside*
/// the container namespace, so the `[ -e ]` checks see the container rootfs
/// (not the host). For each var in [`CA_BUNDLE_ENV_VARS`], it unsets the var
/// when it is set to a path that does not exist inside the container. Vars that
/// point at a path present in the container (e.g. a shared
/// `/etc/ssl/certs/...`) are left untouched, and any workspace-configured value
/// is re-`export`ed *after* this prelude, so intentional overrides survive.
///
/// `SSL_CERT_DIR` may be a colon-separated directory list (OpenSSL 3+), so it
/// is only unset when *none* of its components exist in the container; the
/// other vars are single file paths.
///
/// The variable names are compile-time constants (never user input), so the
/// generated `eval`/`unset` is not an injection vector.
fn ca_env_scrub_prelude() -> String {
    let names = CA_BUNDLE_ENV_VARS.join(" ");
    format!(
        "for __oa_ca in {names}; do \
         eval \"__oa_ca_val=\\${{$__oa_ca:-}}\"; \
         [ -n \"$__oa_ca_val\" ] || continue; \
         if [ \"$__oa_ca\" = SSL_CERT_DIR ]; then \
         __oa_ca_keep=; __oa_ca_rest=$__oa_ca_val; \
         while [ -n \"$__oa_ca_rest\" ]; do \
         __oa_ca_dir=${{__oa_ca_rest%%:*}}; \
         case $__oa_ca_rest in *:*) __oa_ca_rest=${{__oa_ca_rest#*:}};; *) __oa_ca_rest=;; esac; \
         if [ -n \"$__oa_ca_dir\" ] && [ -e \"$__oa_ca_dir\" ]; then __oa_ca_keep=1; fi; \
         done; \
         if [ -z \"$__oa_ca_keep\" ]; then unset SSL_CERT_DIR; fi; \
         elif [ ! -e \"$__oa_ca_val\" ]; then unset \"$__oa_ca\"; fi; \
         done; unset __oa_ca __oa_ca_val __oa_ca_keep __oa_ca_rest __oa_ca_dir; "
    )
}

fn environ_has_keepalive_marker(environ: &[u8]) -> bool {
    let expected = format!(
        "{}={}",
        CONTAINER_KEEPALIVE_ENV_KEY, CONTAINER_KEEPALIVE_ENV_VALUE
    );
    environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected.as_bytes())
}

fn append_nsenter_target_root_arg(args: &mut Vec<String>, enabled: bool) {
    if enabled {
        args.push("--root".to_string());
    }
}

/// Stable container identity for a workspace, derived from its path. Every
/// transient scope a workspace spawns (mission boot, per-exec attach, and the
/// one-shot nspawn wrapper in `nspawn.rs`) embeds this token, so scope
/// discovery (`list_workspace_scope_units`) can match them all by substring.
/// Keep all scope-naming sites in sync with this — a divergent token makes a
/// scope invisible to live stats, retune, and the OOM watchdog.
pub fn machine_name_for_path(path: &Path) -> Option<String> {
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    let sanitized: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let suffix = format!("{:08x}", hasher.finish() as u32);

    Some(format!("sandboxed-{}-{}", sanitized, suffix))
}

/// Build a valid transient systemd scope unit name for a mission container.
/// systemd unit names allow `[A-Za-z0-9:._-]`; replace anything else with `_`.
fn mission_scope_unit(machine_name: &str) -> String {
    let sanitized: String = machine_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("sandboxed-mission-{sanitized}.scope")
}

/// Mission/task tag derived from an exec working directory. Per-mission dirs
/// are `.../workspaces/mission-<first-8-hex-of-uuid>/…` (see
/// `workspace::mission_workspace_dir_for_root`), so the tag is recoverable
/// from any cwd inside a mission workspace. It is embedded in exec scope
/// unit names so mission-end teardown and the zombie reaper can select one
/// mission's scopes without a process registry.
pub fn mission_tag_from_path(cwd: &Path) -> Option<String> {
    let mut below_workspaces = false;
    for comp in cwd.components() {
        let Some(name) = comp.as_os_str().to_str() else {
            below_workspaces = false;
            continue;
        };
        if below_workspaces {
            for (prefix, tag) in [("mission-", 'm'), ("task-", 't')] {
                if let Some(rest) = name.strip_prefix(prefix) {
                    if rest.len() == 8 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Some(format!("{tag}{}", rest.to_ascii_lowercase()));
                    }
                }
            }
        }
        below_workspaces = name == "workspaces";
    }
    None
}

/// Unit name for a per-exec transient scope:
/// `sandboxed-exec-<machine>[-m<8hex>|-t<8hex>]-<rand8>`.
/// The machine token comes first (workspace-level substring discovery by
/// `list_workspace_scope_units` stays intact), the optional mission/task tag
/// second (per-mission teardown), and a short random suffix for uniqueness.
pub fn exec_scope_unit(machine_name: &str, cwd: Option<&Path>) -> String {
    exec_scope_unit_for_mission(machine_name, cwd, None)
}

fn exec_scope_unit_for_mission(
    machine_name: &str,
    cwd: Option<&Path>,
    mission_id: Option<uuid::Uuid>,
) -> String {
    let rand = uuid::Uuid::new_v4().simple().to_string();
    let rand8 = &rand[..8];
    let tag = mission_id
        .map(|id| format!("m{}", id.simple()))
        .or_else(|| cwd.and_then(mission_tag_from_path));
    match tag {
        Some(tag) => format!("sandboxed-exec-{machine_name}-{tag}-{rand8}"),
        None => format!("sandboxed-exec-{machine_name}-{rand8}"),
    }
}

/// Unit name for a durable job scope. Durable scopes deliberately use a
/// separate prefix and contain no mission tag: mission-end teardown only owns
/// `sandboxed-exec-*`, while the durable-job registry owns and cancels these
/// scopes explicitly.
pub fn durable_scope_unit(machine_name: &str, job_id: uuid::Uuid) -> String {
    format!("sandboxed-durable-{machine_name}-{}", job_id.simple())
}

/// Recover the 8-hex mission short id from an exec scope unit name produced
/// by [`exec_scope_unit`]. Parses from the END (`…-m<8hex>-<rand8>.scope`) so
/// machine-name segments can never false-positive. Returns `None` for
/// legacy-named units (pre-mission-tag) and task-tagged units.
pub fn mission_short_id_from_exec_unit(unit: &str) -> Option<String> {
    let name = unit.strip_suffix(".scope").unwrap_or(unit);
    let mut segs = name.rsplit('-');
    let _rand = segs.next()?;
    let tag = segs.next()?;
    let rest = tag.strip_prefix('m')?;
    if matches!(rest.len(), 8 | 32) && rest.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(rest[..8].to_string())
    } else {
        None
    }
}

/// Exact ownership check for newly generated exec scopes. Legacy 8-hex tags
/// intentionally do not match: stopping them immediately would reintroduce
/// the collision this check prevents; the ownership-aware periodic reaper
/// handles those old scopes instead.
pub fn exec_unit_belongs_to_mission(unit: &str, mission_id: uuid::Uuid) -> bool {
    let name = unit.strip_suffix(".scope").unwrap_or(unit);
    let mut segments = name.rsplit('-');
    let _random = segments.next();
    let Some(tag) = segments.next().and_then(|tag| tag.strip_prefix('m')) else {
        return false;
    };
    tag.len() == 32
        && tag.eq_ignore_ascii_case(&mission_id.simple().to_string())
        && tag.chars().all(|c| c.is_ascii_hexdigit())
}

/// Recover the exact machine token from a per-exec scope name. Parsing from
/// the right avoids ambiguous substring/prefix ownership matches when two
/// user-controlled workspace names overlap.
pub fn machine_name_from_exec_unit(unit: &str) -> Option<String> {
    let name = unit
        .strip_suffix(".scope")
        .unwrap_or(unit)
        .strip_prefix("sandboxed-exec-")?;
    let (before_rand, rand) = name.rsplit_once('-')?;
    if !matches!(rand.len(), 8 | 32) || !rand.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if let Some((machine, tag)) = before_rand.rsplit_once('-') {
        let tagged = matches!(tag.len(), 9 | 33)
            && matches!(tag.as_bytes().first(), Some(b'm' | b't'))
            && tag[1..].chars().all(|c| c.is_ascii_hexdigit());
        if tagged {
            return Some(machine.to_string());
        }
    }
    Some(before_rand.to_string())
}

/// systemd slice that hosts every mission scope. Putting all mission scopes
/// under one slice makes them cgroup *siblings* of the API service instead of
/// children, and lets ops pin an **aggregate** memory cap on the slice so the
/// sum of missions can never starve the host or the API — the per-mission cap
/// alone doesn't protect against N missions × cap > RAM.
///
/// Override with `MISSION_SLICE` (empty / `none` / `off` disables the slice
/// assignment for rollback). systemd auto-creates the slice on first use; a
/// unit file or `systemctl set-property missions.slice MemoryHigh=…` pins the
/// aggregate limits.
fn missions_slice() -> Option<String> {
    match std::env::var("MISSION_SLICE") {
        Ok(v) => {
            let v = v.trim().to_string();
            if v.is_empty() || v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("off") {
                None
            } else {
                Some(v)
            }
        }
        Err(_) => Some("missions.slice".to_string()),
    }
}

/// Resource caps applied to a mission's transient scopes (memory + CPU).
///
/// Resolution order (per key): workspace `env_vars` override → API process
/// env. The workspace override is what makes "boost this one workspace"
/// possible without touching the global env file or restarting anything.
///
/// The CPU caps are what keep one mission's runaway build (e.g. a Lean/proof
/// fan-out spawning dozens of parallel jobs) from saturating every core and
/// starving the harness's own response streaming. `missions.slice` carries the
/// *aggregate* CPU guard (a slice-level `CPUWeight`/`CPUQuota` drop-in, lower
/// than `system.slice` where the API service lives, so missions yield to the
/// API tier under contention); these per-scope caps are the *per-mission*
/// guard inside that envelope. Emitting any CPU property also makes systemd
/// delegate the `cpu` controller into `missions.slice`, without which a
/// per-scope `CPUQuota` would silently not be enforced.
#[derive(Debug, Clone, Default)]
pub struct MissionResourceCaps {
    pub max: Option<String>,
    pub high: Option<String>,
    pub swap_max: Option<String>,
    /// `CPUWeight` (1..=10000, or "idle"). Relative share under contention.
    pub cpu_weight: Option<String>,
    /// `CPUQuota` (e.g. "400%" = 4 cores, or "infinity"). Hard per-mission
    /// ceiling so a single mission can never take the whole host.
    pub cpu_quota: Option<String>,
}

impl MissionResourceCaps {
    /// `true` when no cap of any kind is configured — scope wrapping is
    /// skipped entirely (Docker installs without systemd PID 1, dev hosts, …).
    pub fn is_disabled(&self) -> bool {
        self.max.is_none()
            && self.high.is_none()
            && self.swap_max.is_none()
            && self.cpu_weight.is_none()
            && self.cpu_quota.is_none()
    }

    /// `systemd-run` arguments for a transient scope carrying these caps.
    /// Returns `None` when no cap is configured.
    pub(crate) fn scope_run_args(&self, unit: &str) -> Option<Vec<String>> {
        if self.is_disabled() {
            return None;
        }
        let mut args = vec![
            "--scope".to_string(),
            "--quiet".to_string(),
            "--collect".to_string(),
            format!("--unit={unit}"),
        ];
        if let Some(slice) = missions_slice() {
            args.push(format!("--slice={slice}"));
        }
        if let Some(max) = &self.max {
            args.push(format!("--property=MemoryMax={max}"));
        }
        if let Some(high) = &self.high {
            args.push(format!("--property=MemoryHigh={high}"));
        }
        if let Some(swap) = &self.swap_max {
            args.push(format!("--property=MemorySwapMax={swap}"));
        }
        if let Some(weight) = &self.cpu_weight {
            args.push(format!("--property=CPUWeight={weight}"));
        }
        if let Some(quota) = &self.cpu_quota {
            args.push(format!("--property=CPUQuota={quota}"));
        }
        Some(args)
    }
}

/// Build [`MissionResourceCaps`] from an arbitrary env map (workspace env vars,
/// `NspawnConfig::env`, …) with process-env fallback.
pub(crate) fn mission_resource_caps_from_env(env: &HashMap<String, String>) -> MissionResourceCaps {
    MissionResourceCaps {
        max: resolve_resource_var(env, "MISSION_MEMORY_MAX"),
        high: resolve_resource_var(env, "MISSION_MEMORY_HIGH"),
        swap_max: resolve_resource_var(env, "MISSION_MEMORY_SWAP_MAX"),
        cpu_weight: resolve_resource_var(env, "MISSION_CPU_WEIGHT"),
        cpu_quota: resolve_resource_var(env, "MISSION_CPU_QUOTA"),
    }
}

/// Resolve one resource-cap key: workspace env override first, then process
/// env. An empty workspace value is treated as "no override" and falls back to
/// the process default — same as removing the key — so clearing a cap via the
/// raw env editor and via the Resources "Reset to defaults" button behave
/// identically, and neither silently drops the mission out of its cgroup scope.
/// To run a single workspace genuinely uncapped, set the value to `"infinity"`
/// (memory) or `"infinity"`/`10000` (CPU), which still wraps the mission in a
/// discoverable scope.
fn resolve_resource_var(workspace_env: &HashMap<String, String>, key: &str) -> Option<String> {
    if let Some(v) = workspace_env.get(key) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn select_container_resolv_conf() -> Option<PathBuf> {
    let default_path = PathBuf::from("/etc/resolv.conf");
    let content = fs::read_to_string(&default_path).ok()?;
    let is_stub = content.contains("127.0.0.53") || content.contains("127.0.0.1");
    if !is_stub {
        return Some(default_path);
    }

    let search_line = content
        .lines()
        .find(|line| line.starts_with("search ") || line.starts_with("domain "))
        .map(str::to_string);
    let include_tailnet = content.contains(".ts.net") || content.contains("tailscale");

    let resolved = synthesized_container_resolv_conf(search_line.as_deref(), include_tailnet);

    let custom_path = PathBuf::from("/var/lib/opencode/.sandboxed-sh/resolv.conf");
    if let Some(parent) = custom_path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return Some(default_path);
        }
    }
    if fs::write(&custom_path, resolved).is_err() {
        return Some(default_path);
    }

    Some(custom_path)
}

fn synthesized_container_resolv_conf(search_line: Option<&str>, include_tailnet: bool) -> String {
    let mut resolved = String::new();
    if let Some(line) = search_line {
        resolved.push_str(line);
        resolved.push('\n');
    }
    resolved.push_str("nameserver 1.1.1.1\n");
    resolved.push_str("nameserver 8.8.8.8\n");
    if include_tailnet {
        resolved.push_str("nameserver 100.100.100.100\n");
    }
    resolved
}

fn bind_resolv_conf(cmd: &mut Command) {
    if let Some(path) = select_container_resolv_conf() {
        push_resolv_conf_bind_args(cmd, &path);
    }
}

fn push_resolv_conf_bind_args(cmd: &mut Command, path: &Path) {
    for arg in resolv_conf_bind_args(path) {
        cmd.arg(arg);
    }
}

fn resolv_conf_bind_args(path: &Path) -> Vec<String> {
    let bind_arg = if path == Path::new("/etc/resolv.conf") {
        "--bind-ro=/etc/resolv.conf".to_string()
    } else {
        format!("--bind-ro={}:{}", path.display(), "/etc/resolv.conf")
    };
    vec!["--resolv-conf=off".to_string(), bind_arg]
}

/// Nspawn arguments binding the container `/etc/resolv.conf`, preferring the
/// synthesized resolver (public DNS before MagicDNS) when the host resolver is
/// a local stub. Shared with the API preflight path so both avoid binding a
/// MagicDNS-first resolver into containers.
pub(crate) fn resolv_conf_nspawn_args() -> Vec<String> {
    select_container_resolv_conf()
        .map(|path| resolv_conf_bind_args(&path))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        append_nsenter_target_root_arg, ca_env_scrub_prelude, durable_scope_unit,
        environ_has_keepalive_marker, exec_scope_unit, exec_scope_unit_for_mission,
        exec_unit_belongs_to_mission, machine_name_from_exec_unit, mission_short_id_from_exec_unit,
        mission_tag_from_path, normalize_container_path, replace_command_env,
        resolv_conf_bind_args, synthesized_container_resolv_conf, WorkspaceExec,
        CA_BUNDLE_ENV_VARS,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use tokio::process::Command;

    #[test]
    fn mission_tag_extracted_from_workspace_cwd() {
        assert_eq!(
            mission_tag_from_path(Path::new(
                "/root/.sandboxed-sh/containers/dumbcontracts/workspaces/mission-4efda364/repo"
            ))
            .as_deref(),
            Some("m4efda364")
        );
        assert_eq!(
            mission_tag_from_path(Path::new("/workspaces/task-deadbeef")).as_deref(),
            Some("tdeadbeef")
        );
        // Not 8 hex chars → no tag.
        assert_eq!(
            mission_tag_from_path(Path::new("/workspaces/mission-xyz")),
            None
        );
        assert_eq!(
            mission_tag_from_path(Path::new("/workspaces/mission-123456789")),
            None
        );
        // User-controlled parent names must not override the actual mission
        // directory immediately below `workspaces`.
        assert_eq!(
            mission_tag_from_path(Path::new(
                "/srv/mission-deadbeef/containers/workspaces/mission-4EFDA364/repo"
            ))
            .as_deref(),
            Some("m4efda364")
        );
        assert_eq!(
            mission_tag_from_path(Path::new("/srv/mission-deadbeef/repo")),
            None
        );
        assert_eq!(mission_tag_from_path(Path::new("/srv/monorepo")), None);
    }

    #[test]
    fn exec_scope_unit_roundtrips_mission_short_id() {
        let unit = exec_scope_unit(
            "sandboxed-dumbcontracts-634e6d35",
            Some(Path::new("/workspaces/mission-4efda364")),
        );
        assert!(unit.starts_with("sandboxed-exec-sandboxed-dumbcontracts-634e6d35-m4efda364-"));
        assert_eq!(
            mission_short_id_from_exec_unit(&format!("{unit}.scope")).as_deref(),
            Some("4efda364")
        );
        assert_eq!(
            mission_short_id_from_exec_unit(&unit).as_deref(),
            Some("4efda364")
        );

        // Legacy naming (32-char random suffix, no tag) must not parse — the
        // machine hash segment is 8 hex but lacks the `m` prefix.
        assert_eq!(
            mission_short_id_from_exec_unit(
                "sandboxed-exec-sandboxed-dumbcontracts-634e6d35-000b8543a2244e49875a6bf64594dbc5.scope"
            ),
            None
        );
        // Task tags are not mission ids.
        let task_unit = exec_scope_unit(
            "sandboxed-misc-deadbeef",
            Some(Path::new("/workspaces/task-01234567")),
        );
        assert_eq!(mission_short_id_from_exec_unit(&task_unit), None);
        // No cwd → no tag, still unique-suffixed.
        let bare = exec_scope_unit("sandboxed-misc-deadbeef", None);
        assert_eq!(mission_short_id_from_exec_unit(&bare), None);
        assert!(bare.starts_with("sandboxed-exec-sandboxed-misc-deadbeef-"));

        assert_eq!(
            machine_name_from_exec_unit(&format!("{unit}.scope")).as_deref(),
            Some("sandboxed-dumbcontracts-634e6d35")
        );
        assert_eq!(
            machine_name_from_exec_unit(&task_unit).as_deref(),
            Some("sandboxed-misc-deadbeef")
        );
        assert_eq!(
            machine_name_from_exec_unit(&bare).as_deref(),
            Some("sandboxed-misc-deadbeef")
        );
        assert_eq!(
            machine_name_from_exec_unit(
                "sandboxed-exec-sandboxed-dumbcontracts-634e6d35-000b8543a2244e49875a6bf64594dbc5.scope"
            )
            .as_deref(),
            Some("sandboxed-dumbcontracts-634e6d35")
        );
    }

    #[test]
    fn durable_scope_is_not_owned_by_mission_reaper() {
        let id = uuid::Uuid::parse_str("df71826d-2060-44bd-b674-0ab16083e3b3").unwrap();
        let unit = durable_scope_unit("sandboxed-dumbcontracts-634e6d35", id);

        assert_eq!(
            unit,
            "sandboxed-durable-sandboxed-dumbcontracts-634e6d35-df71826d206044bdb6740ab16083e3b3"
        );
        assert_eq!(mission_short_id_from_exec_unit(&unit), None);
        assert!(!unit.starts_with("sandboxed-exec-"));
    }

    #[tokio::test]
    async fn durable_host_environment_replaces_service_environment() {
        let mut cmd = Command::new("/usr/bin/env");
        cmd.env("API_ONLY_SECRET", "must-not-leak");
        replace_command_env(
            &mut cmd,
            HashMap::from([("WORKSPACE_VALUE".to_string(), "visible".to_string())]),
        );

        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.lines().any(|line| line == "WORKSPACE_VALUE=visible"));
        assert!(!stdout.contains("API_ONLY_SECRET"));
        assert!(!stdout.contains("must-not-leak"));
    }

    #[test]
    fn full_mission_scope_tag_prevents_short_id_collisions() {
        let first = uuid::Uuid::parse_str("deadbeef-0000-4000-8000-000000000001").unwrap();
        let collision = uuid::Uuid::parse_str("deadbeef-0000-4000-8000-000000000002").unwrap();
        let unit = exec_scope_unit_for_mission(
            "sandboxed-dumbcontracts-634e6d35",
            Some(Path::new("/workspaces/mission-deadbeef")),
            Some(first),
        );

        assert!(exec_unit_belongs_to_mission(&unit, first));
        assert!(!exec_unit_belongs_to_mission(&unit, collision));
        assert_eq!(
            mission_short_id_from_exec_unit(&unit).as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            machine_name_from_exec_unit(&format!("{unit}.scope")).as_deref(),
            Some("sandboxed-dumbcontracts-634e6d35")
        );
        let legacy = exec_scope_unit(
            "sandboxed-dumbcontracts-634e6d35",
            Some(Path::new("/workspaces/mission-deadbeef")),
        );
        assert!(!exec_unit_belongs_to_mission(&legacy, first));
    }

    #[test]
    fn synthesized_resolv_conf_uses_public_dns_before_magic_dns() {
        let resolv = synthesized_container_resolv_conf(Some("search gazella-vector.ts.net"), true);

        assert_eq!(
            resolv,
            "search gazella-vector.ts.net\n\
             nameserver 1.1.1.1\n\
             nameserver 8.8.8.8\n\
             nameserver 100.100.100.100\n"
        );
    }

    #[test]
    fn resolv_conf_bind_disables_nspawn_resolver_initialization() {
        assert_eq!(
            resolv_conf_bind_args(Path::new("/tmp/sandboxed-resolv.conf")),
            vec![
                "--resolv-conf=off",
                "--bind-ro=/tmp/sandboxed-resolv.conf:/etc/resolv.conf",
            ]
        );
    }

    #[test]
    fn container_path_adds_system_dirs_when_missing() {
        let path = normalize_container_path(Some("/root/.elan/bin"));

        assert!(path.starts_with("/root/.elan/bin:"));
        assert!(path.contains(":/usr/bin:"));
        assert!(path.ends_with(":/bin"));
        assert!(path.contains(":/root/.bun/bin:"));
    }

    #[test]
    fn container_path_preserves_existing_priority_without_duplicates() {
        let path = normalize_container_path(Some("/tmp/wrapper:/usr/bin:/tmp/wrapper"));
        let dirs: Vec<_> = path.split(':').collect();

        assert_eq!(dirs[0], "/tmp/wrapper");
        assert_eq!(dirs.iter().filter(|dir| **dir == "/usr/bin").count(), 1);
        assert_eq!(dirs.iter().filter(|dir| **dir == "/tmp/wrapper").count(), 1);
    }

    #[test]
    fn ca_scrub_prelude_covers_all_ca_bundle_vars() {
        let prelude = ca_env_scrub_prelude();
        for name in CA_BUNDLE_ENV_VARS {
            assert!(
                prelude.contains(name),
                "prelude should reference {name}: {prelude}"
            );
        }
        // Only unsets when the referenced path is absent inside the container.
        assert!(prelude.contains("[ ! -e "));
        assert!(prelude.contains("unset "));
    }

    /// Runs the generated prelude in a real POSIX shell to verify scrub
    /// semantics, including colon-separated `SSL_CERT_DIR` lists (OpenSSL 3+):
    /// the var must survive if any component exists in the container.
    #[test]
    fn ca_scrub_prelude_shell_semantics() {
        let run = |env: &[(&str, &str)]| -> String {
            let script = format!(
                "{} echo \"${{SSL_CERT_FILE:-UNSET}}|${{SSL_CERT_DIR:-UNSET}}\"",
                ca_env_scrub_prelude()
            );
            let mut cmd = std::process::Command::new("/bin/sh");
            // The test runner's own environment may carry CA vars (the very
            // leak this prelude fixes), so start from a clean slate.
            cmd.env_clear();
            cmd.arg("-c").arg(&script);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("run /bin/sh");
            assert!(out.status.success(), "prelude script failed: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // Broken single-path var is scrubbed; existing one is kept.
        assert_eq!(
            run(&[
                ("SSL_CERT_FILE", "/nonexistent/cacert.pem"),
                ("SSL_CERT_DIR", "/etc"),
            ]),
            "UNSET|/etc"
        );
        // Colon-separated SSL_CERT_DIR survives when any component exists.
        assert_eq!(
            run(&[("SSL_CERT_DIR", "/nonexistent-dir:/etc")]),
            "UNSET|/nonexistent-dir:/etc"
        );
        // ...and is scrubbed only when no component exists.
        assert_eq!(
            run(&[("SSL_CERT_DIR", "/nonexistent-a:/nonexistent-b")]),
            "UNSET|UNSET"
        );
    }

    #[test]
    fn shell_command_scrubs_ca_env_before_exporting_workspace_vars() {
        let mut env = HashMap::new();
        env.insert(
            "SSL_CERT_FILE".to_string(),
            "/root/workspace-ca.pem".to_string(),
        );
        let cmd = WorkspaceExec::build_shell_command_with_env(
            "/w",
            "/usr/local/bin/claude",
            &[],
            Some(&env),
        );

        let scrub_at = cmd.find("__oa_ca").expect("scrub prelude present");
        let export_at = cmd
            .find("export SSL_CERT_FILE=")
            .expect("workspace SSL_CERT_FILE re-exported after scrub");
        // Prelude runs first; the intentional workspace override is applied
        // afterwards so it survives the scrub.
        assert!(
            scrub_at < export_at,
            "scrub must precede workspace exports: {cmd}"
        );
        assert!(cmd.contains("exec '/usr/local/bin/claude'"));
    }

    #[test]
    fn shell_command_without_env_still_scrubs_ca_vars() {
        // Even with no workspace env (preflight uses an empty map), the nsenter
        // shell must still drop inherited-but-broken CA vars.
        let cmd = WorkspaceExec::build_shell_command_with_env("/w", "curl", &[], None);
        assert!(cmd.contains("SSL_CERT_FILE"));
        assert!(cmd.contains("NODE_EXTRA_CA_CERTS"));
        assert!(cmd.trim_start().starts_with("for __oa_ca in"));
    }

    #[test]
    fn keepalive_marker_is_detected_in_proc_environ_bytes() {
        assert!(environ_has_keepalive_marker(
            b"PATH=/usr/bin\0SANDBOXED_SH_CONTAINER_KEEPALIVE=1\0HOME=/root\0"
        ));
        assert!(!environ_has_keepalive_marker(
            b"PATH=/usr/bin\0SANDBOXED_SH_CONTAINER_KEEPALIVE=0\0HOME=/root\0"
        ));
    }

    #[test]
    fn pty_nsenter_target_root_guard_precedes_shell_payload() {
        let mut args = vec!["--pid".to_string()];
        append_nsenter_target_root_arg(&mut args, true);
        args.extend(["/bin/sh".to_string(), "-lc".to_string()]);
        assert_eq!(args, ["--pid", "--root", "/bin/sh", "-lc"]);

        let mut disabled = vec!["--pid".to_string()];
        append_nsenter_target_root_arg(&mut disabled, false);
        assert_eq!(disabled, ["--pid"]);
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceExec {
    pub workspace: Workspace,
}

/// Child process spawned inside a PTY.
///
/// On Unix, both Host and nspawn Container workspaces use raw `openpty()` for
/// compatibility with CLI tools that hang under portable-pty. portable-pty
/// 0.9's spawn_command resets signal dispositions and sweeps random fds in
/// pre_exec, which causes Claude Code CLI to silently hang producing no PTY
/// output. Raw openpty with a minimal `setsid`/`TIOCSCTTY` pre_exec avoids
/// this. Non-nspawn containers and non-Unix hosts still use portable-pty.
pub struct PtyChild {
    child: PtyChildProcess,
    master: PtyMasterHandle,
}

enum PtyChildProcess {
    PortablePty(Box<dyn portable_pty::Child + Send + Sync>),
    #[cfg(unix)]
    Std(std::process::Child),
}

enum PtyMasterHandle {
    PortablePty(Box<dyn portable_pty::MasterPty + Send>),
    #[cfg(unix)]
    Unix(std::os::unix::io::OwnedFd),
}

impl PtyChild {
    pub fn kill(&mut self) {
        match &mut self.child {
            PtyChildProcess::PortablePty(c) => {
                let _ = c.kill();
            }
            #[cfg(unix)]
            PtyChildProcess::Std(c) => {
                let _ = c.kill();
            }
        }
    }

    pub fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        match &self.master {
            PtyMasterHandle::PortablePty(m) => Ok(m.take_writer()?),
            #[cfg(unix)]
            PtyMasterHandle::Unix(fd) => {
                use std::os::unix::io::{AsRawFd, FromRawFd};
                // SAFETY: fd.as_raw_fd() returns a valid descriptor owned by
                // PtyMasterHandle; dup() produces a new independent fd.
                let duped = unsafe { libc::dup(fd.as_raw_fd()) };
                if duped < 0 {
                    anyhow::bail!(
                        "dup() for PTY writer failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                // SAFETY: duped is a valid fd (checked above) and sole
                // ownership is transferred to the File.
                Ok(Box::new(unsafe { std::fs::File::from_raw_fd(duped) }))
            }
        }
    }

    pub fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        match &self.master {
            PtyMasterHandle::PortablePty(m) => Ok(m.try_clone_reader()?),
            #[cfg(unix)]
            PtyMasterHandle::Unix(fd) => {
                use std::os::unix::io::{AsRawFd, FromRawFd};
                // SAFETY: fd.as_raw_fd() returns a valid descriptor owned by
                // PtyMasterHandle; dup() produces a new independent fd.
                let duped = unsafe { libc::dup(fd.as_raw_fd()) };
                if duped < 0 {
                    anyhow::bail!(
                        "dup() for PTY reader failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                // SAFETY: duped is a valid fd (checked above) and sole
                // ownership is transferred to the File.
                Ok(Box::new(unsafe { std::fs::File::from_raw_fd(duped) }))
            }
        }
    }

    /// Return the PID of the child process, if available.
    pub fn process_id(&self) -> Option<u32> {
        match &self.child {
            PtyChildProcess::PortablePty(c) => c.process_id(),
            #[cfg(unix)]
            PtyChildProcess::Std(c) => Some(c.id()),
        }
    }

    /// Disable canonical mode and echo on the PTY line discipline.
    ///
    /// Needed when feeding structured input (stream-json) through the PTY:
    /// canonical mode truncates lines at 4096 bytes and echo would reflect
    /// every injected line back into the child's stdout stream that we parse.
    #[cfg(unix)]
    pub fn set_raw_input_mode(&self) -> std::io::Result<()> {
        use std::os::unix::io::AsRawFd;
        let fd = match &self.master {
            PtyMasterHandle::PortablePty(m) => m
                .as_raw_fd()
                .ok_or_else(|| std::io::Error::other("PTY master has no raw fd"))?,
            PtyMasterHandle::Unix(fd) => fd.as_raw_fd(),
        };
        // SAFETY: fd is a valid open PTY master descriptor owned by self.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut termios) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            termios.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ECHONL);
            if libc::tcsetattr(fd, libc::TCSANOW, &termios) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Wait for the child process to exit. Must be called from a blocking context.
    pub fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        match &mut self.child {
            PtyChildProcess::PortablePty(c) => c.wait(),
            #[cfg(unix)]
            PtyChildProcess::Std(c) => Ok(c.wait()?.into()),
        }
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        // Kill the child if still running, then reap to avoid zombies.
        self.kill();
        #[cfg(unix)]
        if let PtyChildProcess::Std(ref mut c) = self.child {
            let _ = c.wait();
        }
    }
}

impl WorkspaceExec {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }

    /// Translate a host path to a container-relative path.
    ///
    /// For container workspaces using nspawn/nsenter, paths must be relative to the container
    /// filesystem root, not the host. This translates paths like:
    ///   /root/.sandboxed-sh/containers/minecraft/<workspace>/.claude/settings.json
    /// to:
    ///   /workspaces/<workspace>/.claude/settings.json
    ///
    /// For host workspaces or fallback mode, returns the original path unchanged.
    pub fn translate_path_for_container(&self, path: &Path) -> String {
        if self.workspace.workspace_type != WorkspaceType::Container {
            return path.to_string_lossy().to_string();
        }
        if !use_nspawn_for_workspace(&self.workspace) {
            return path.to_string_lossy().to_string();
        }
        // Translate to container-relative path
        self.rel_path_in_container(path)
    }

    fn rel_path_in_container(&self, cwd: &Path) -> String {
        let root = &self.workspace.path;
        let rel = cwd.strip_prefix(root).unwrap_or_else(|_| Path::new(""));
        if rel.as_os_str().is_empty() {
            "/".to_string()
        } else {
            format!("/{}", rel.to_string_lossy())
        }
    }

    fn build_env(&self, extra_env: HashMap<String, String>) -> HashMap<String, String> {
        let mut merged = self.workspace.env_vars.clone();
        merged.extend(extra_env);
        merged
            .entry("SANDBOXED_SH_WORKSPACE_TYPE".to_string())
            .or_insert_with(|| self.workspace.workspace_type.as_str().to_string());
        if self.workspace.workspace_type == WorkspaceType::Container {
            if let Some(name) = self.workspace.path.file_name().and_then(|n| n.to_str()) {
                if !name.trim().is_empty() {
                    merged
                        .entry("SANDBOXED_SH_WORKSPACE_NAME".to_string())
                        .or_insert_with(|| name.to_string());
                }
            }
            // Ensure container processes use container-local XDG paths instead of host defaults.
            merged
                .entry("HOME".to_string())
                .or_insert_with(|| "/root".to_string());
            merged
                .entry("XDG_CONFIG_HOME".to_string())
                .or_insert_with(|| "/root/.config".to_string());
            merged
                .entry("XDG_DATA_HOME".to_string())
                .or_insert_with(|| "/root/.local/share".to_string());
            merged
                .entry("XDG_STATE_HOME".to_string())
                .or_insert_with(|| "/root/.local/state".to_string());
            merged
                .entry("XDG_CACHE_HOME".to_string())
                .or_insert_with(|| "/root/.cache".to_string());

            let normalized_path = normalize_container_path(merged.get("PATH").map(String::as_str));
            merged.insert("PATH".to_string(), normalized_path);
        }
        if self.workspace.workspace_type == WorkspaceType::Container
            && !use_nspawn_for_workspace(&self.workspace)
        {
            merged
                .entry("SANDBOXED_SH_CONTAINER_FALLBACK".to_string())
                .or_insert_with(|| "1".to_string());
        }

        // Make GitHub auth survive per-mission HOME divergence. The credential
        // injection writes ~/.git-credentials, ~/.gitconfig, and
        // ~/.config/gh/hosts.yml to the workspace home — but some backends
        // (claudecode) launch the agent with HOME/XDG_CONFIG_HOME pointed at a
        // per-mission dir. Export non-secret pointers to those files so both
        // `gh` and `git` can still find them.
        let git_creds = self.workspace.resolved_git_credentials.clone().or_else(|| {
            crate::workspace::git_credentials::GitCredentialConfig::resolve(
                &self.workspace.path,
                None,
            )
        });
        if let Some(creds) = git_creds {
            creds.apply_to_env(
                &mut merged,
                &self.workspace.path,
                self.workspace.workspace_type,
                &self.workspace.env_vars,
            );
        }

        merged
    }

    fn shell_escape(value: &str) -> String {
        if value.is_empty() {
            return "''".to_string();
        }
        let mut escaped = String::new();
        escaped.push('\'');
        for ch in value.chars() {
            if ch == '\'' {
                escaped.push_str("'\"'\"'");
            } else {
                escaped.push(ch);
            }
        }
        escaped.push('\'');
        escaped
    }

    fn valid_env_key(key: &str) -> bool {
        !key.trim().is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    /// Build a shell command with optional env var exports.
    /// When `env` is provided, all env vars are exported before running the
    /// program. nsenter callers intentionally pass `None` and use the child
    /// process environment so credentials never appear in argv.
    fn build_shell_command_with_env(
        rel_cwd: &str,
        program: &str,
        args: &[String],
        env: Option<&HashMap<String, String>>,
    ) -> String {
        let mut cmd = String::new();

        // Drop inherited-but-broken TLS CA env vars (e.g. a host venv
        // SSL_CERT_FILE) before exporting workspace vars, so they can't break
        // HTTPS inside the container. Runs first so workspace-configured CA
        // vars re-exported below still win.
        cmd.push_str(&ca_env_scrub_prelude());

        // Export env vars inside the shell command so they're available in the container.
        // Keys are validated to POSIX env var names (alphanumeric + underscore) to prevent
        // shell injection via crafted env var keys. Values are single-quote escaped.
        if let Some(env) = env {
            for (k, v) in env {
                if k.trim().is_empty() {
                    continue;
                }
                if !Self::valid_env_key(k) {
                    tracing::warn!(key = %k, "Skipping env var with invalid key characters");
                    continue;
                }
                cmd.push_str("export ");
                cmd.push_str(k);
                cmd.push('=');
                cmd.push_str(&Self::shell_escape(v));
                cmd.push_str("; ");
            }
        }

        cmd.push_str("cd ");
        cmd.push_str(&Self::shell_escape(rel_cwd));
        cmd.push_str(" && exec ");
        cmd.push_str(&Self::shell_escape(program));
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&Self::shell_escape(arg));
        }
        cmd
    }

    /// Build a shell command that bootstraps Tailscale networking before running the program.
    ///
    /// This runs the sandboxed-tailscale-up script (which also calls sandboxed-network-up)
    /// to bring up the veth interface, get an IP via DHCP, start tailscaled, and authenticate.
    /// The scripts are installed by the workspace template's init_script.
    ///
    /// When `export_all_env` is true (nsenter path), all env vars are exported in the
    /// shell command since nsenter doesn't propagate env vars into the namespace.
    /// When false (nspawn path), only TS_* vars are exported (others use --setenv).
    ///
    /// `tailnet_only`: When true, set up default route via host gateway for internet
    /// while using Tailscale only for tailnet device access. When false (exit_node mode),
    /// all traffic goes through Tailscale's exit node.
    fn build_tailscale_bootstrap_command(
        rel_cwd: &str,
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
        export_all_env: bool,
        tailnet_only: bool,
    ) -> String {
        let mut cmd = String::new();

        // Scrub inherited-but-broken TLS CA env vars before anything else so
        // the Tailscale bootstrap's own HTTPS calls (and the exec'd program)
        // fall back to the container's default CA store. Only meaningful on the
        // nsenter path where env is inherited; harmless under nspawn.
        if export_all_env {
            cmd.push_str(&ca_env_scrub_prelude());
        }

        // Export env vars so the bootstrap script and program can use them.
        for (k, v) in env {
            if k.trim().is_empty() {
                continue;
            }
            // Validate key: only POSIX env var names to prevent shell injection.
            if !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            // When using nsenter, export ALL env vars (nsenter doesn't propagate them).
            // When using nspawn, only export TS_* vars (others are passed via --setenv).
            if export_all_env || (k.starts_with("TS_") && !v.trim().is_empty()) {
                cmd.push_str("export ");
                cmd.push_str(k);
                cmd.push('=');
                cmd.push_str(&Self::shell_escape(v));
                cmd.push_str("; ");
            }
        }

        // Run the Tailscale bootstrap script if it exists.
        // The script calls sandboxed-network-up (DHCP via udhcpc, which sets up
        // the IP, default route, and DNS), then starts tailscaled and authenticates.
        // Errors are suppressed to allow the main program to run even if networking fails.
        cmd.push_str(
            "if [ -x /usr/local/bin/sandboxed-tailscale-up ]; then \
             /usr/local/bin/sandboxed-tailscale-up >/dev/null 2>&1 || true; \
             fi; ",
        );

        if tailnet_only {
            // tailnet_only mode: Use Tailscale for tailnet access only, route
            // regular internet traffic through the host gateway (not exit node).
            // This ensures the container can reach both tailnet devices AND the internet.
            cmd.push_str(
                "_oa_ip=$(ip -4 addr show host0 2>/dev/null | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p' | head -1); \
                 _oa_gw=\"${_oa_ip%.*}.1\"; \
                 if [ -n \"$_oa_ip\" ]; then \
                   ip route del default 2>/dev/null || true; \
                   ip route add default via \"$_oa_gw\" 2>/dev/null || true; \
                 fi; \
                 if [ ! -s /etc/resolv.conf ]; then \
                 printf 'nameserver 8.8.8.8\\nnameserver 1.1.1.1\\n' > /etc/resolv.conf 2>/dev/null || true; \
                 fi; ",
            );
        } else {
            // exit_node mode: Fallback route only if DHCP/Tailscale didn't set one.
            // All traffic should go through Tailscale's exit node when properly configured.
            cmd.push_str(
                "if ! ip route show default 2>/dev/null | grep -q default; then \
                 _oa_ip=$(ip -4 addr show host0 2>/dev/null | sed -n 's/.*inet \\([0-9.]*\\).*/\\1/p' | head -1); \
                 _oa_gw=\"${_oa_ip%.*}.1\"; \
                 [ -n \"$_oa_ip\" ] && ip route add default via \"$_oa_gw\" 2>/dev/null || true; \
                 fi; \
                 if [ ! -s /etc/resolv.conf ]; then \
                 printf 'nameserver 8.8.8.8\\nnameserver 1.1.1.1\\n' > /etc/resolv.conf 2>/dev/null || true; \
                 fi; ",
            );
        }

        // Change to the working directory and exec the main program.
        cmd.push_str("cd ");
        cmd.push_str(&Self::shell_escape(rel_cwd));
        cmd.push_str(" && exec ");
        cmd.push_str(&Self::shell_escape(program));
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&Self::shell_escape(arg));
        }
        cmd
    }

    pub(crate) fn machine_name(&self) -> Option<String> {
        machine_name_for_path(&self.workspace.path)
    }

    /// Resource caps (memory + CPU) for this workspace's mission scopes
    /// (workspace env override → process env; see [`resolve_resource_var`]).
    pub fn mission_resource_caps(&self) -> MissionResourceCaps {
        mission_resource_caps_from_env(&self.workspace.env_vars)
    }

    /// The boot scope unit name for this workspace's container, when one can
    /// be derived. Public so the API layer (live cap adjustment, memory
    /// stats, OOM watchdog) can address the same unit this module creates.
    pub fn mission_boot_scope_unit(&self) -> Option<String> {
        self.machine_name().map(|name| mission_scope_unit(&name))
    }

    /// Token shared by every transient scope this workspace spawns (boot +
    /// per-exec attach scopes). Used by the API layer to `systemctl
    /// set-property` all of a workspace's scopes at once.
    pub fn mission_scope_match_token(&self) -> Option<String> {
        self.machine_name()
    }

    async fn running_container_leader(&self) -> Option<String> {
        // Patched: discover the leader via pgrep instead of machinectl.
        // Inside the docker entrypoint (Caddy as PID 1) machinectl refuses
        // to operate, and machined cannot create cgroup scopes without
        // systemd as PID 1, so we run nspawn with --register=no and locate
        // the leader by scanning for `systemd-nspawn -D <workspace path>`.
        let mut paths = vec![self.workspace.path.to_string_lossy().into_owned()];
        if let Ok(canonical) = self.workspace.path.canonicalize() {
            let canonical = canonical.to_string_lossy().into_owned();
            if !paths.iter().any(|path| path == &canonical) {
                paths.push(canonical);
            }
        }

        let mut nspawn_pid = None;
        for path in paths {
            let output = Command::new("pgrep")
                .args(["-f", &format!("systemd-nspawn.*-D {}", path)])
                .output()
                .await
                .ok()?;
            if output.status.success() {
                nspawn_pid = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                if nspawn_pid.is_some() {
                    break;
                }
            }
        }
        let nspawn_pid = nspawn_pid?;
        let child_pids = Command::new("pgrep")
            .args(["-P", &nspawn_pid])
            .output()
            .await
            .ok()?;
        if !child_pids.status.success() {
            return None;
        }
        let leader = String::from_utf8_lossy(&child_pids.stdout)
            .lines()
            .next()
            .map(|s| s.trim().to_string())?;
        if leader.is_empty() {
            None
        } else {
            Some(leader)
        }
    }

    fn leader_has_keepalive_marker(&self, leader: &str) -> bool {
        let path = format!("/proc/{}/environ", leader);
        std::fs::read(path)
            .map(|bytes| environ_has_keepalive_marker(&bytes))
            .unwrap_or(false)
    }

    async fn start_persistent_container_leader(
        &self,
        env: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        let root = self.workspace.path.clone();
        let name = self
            .machine_name()
            .filter(|name| !name.trim().is_empty())
            .context("Container workspace has no machine name")?;

        // Per-mission isolation (Layer 1). When `MISSION_MEMORY_MAX` is set
        // (workspace env override → process env), boot the container inside
        // its own transient systemd scope — under `missions.slice` so the
        // aggregate of all missions is capped separately from the API
        // service — with memory + swap caps so one mission's runaway build
        // (e.g. a 46G Lean compilation) can't starve the host or the API
        // process, and so `systemctl stop <scope>` reliably kills the whole
        // build tree on cancel/teardown. Unset (default) = unchanged direct
        // nspawn boot, so shipping this binary is a no-op until the env is
        // enabled (Docker installs without systemd PID 1 stay on the direct
        // path).
        let caps = self.mission_resource_caps();
        let scope_args = caps.scope_run_args(&mission_scope_unit(&name));
        let mut cmd = if let Some(scope_args) = scope_args {
            let mut c = Command::new("systemd-run");
            c.args(&scope_args);
            // systemd-nspawn's own scope-creation is disabled below via
            // --register=no/--keep-unit, so it joins this scope's cgroup.
            c.arg("systemd-nspawn");
            c
        } else {
            Command::new("systemd-nspawn")
        };
        cmd.arg("-D").arg(root);
        cmd.arg(format!("--machine={}", name));
        cmd.arg("--quiet");
        cmd.arg("--timezone=off");
        cmd.arg("--console=pipe");
        cmd.arg("--register=no");
        cmd.arg("--keep-unit");

        let context_dir_name = std::env::var("SANDBOXED_SH_CONTEXT_DIR_NAME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "context".to_string());
        let global_context_root = std::env::var("SANDBOXED_SH_CONTEXT_ROOT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("WORKING_DIR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(|dir| PathBuf::from(dir).join(&context_dir_name))
            })
            .unwrap_or_else(|| PathBuf::from("/root").join(&context_dir_name));
        if global_context_root.exists() {
            cmd.arg(format!(
                "--bind={}:/root/context",
                global_context_root.display()
            ));
        }

        let x11_socket_path = Path::new("/tmp/.X11-unix");
        if x11_socket_path.exists() {
            cmd.arg("--bind=/tmp/.X11-unix");
        }

        let fido_agent_path = Path::new("/run/sandboxed-sh/fido-agent.sock");
        if fido_agent_path.exists() {
            cmd.arg("--bind=/run/sandboxed-sh/fido-agent.sock");
        }

        let use_shared_network = self.workspace.shared_network.unwrap_or(true);
        if use_shared_network {
            bind_resolv_conf(&mut cmd);
        } else {
            let tailscale_args = nspawn::tailscale_nspawn_extra_args(env);
            bind_resolv_conf(&mut cmd);
            for arg in tailscale_args {
                cmd.arg(arg);
            }
        }

        cmd.arg(format!(
            "--setenv={}={}",
            CONTAINER_KEEPALIVE_ENV_KEY, CONTAINER_KEEPALIVE_ENV_VALUE
        ));
        cmd.arg("/bin/sh");
        cmd.arg("-lc");
        cmd.arg("trap 'exit 0' TERM INT; while :; do sleep 3600 & wait $!; done");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .context("Failed to start persistent container leader")?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(())
    }

    async fn ensure_persistent_container_leader(
        &self,
        env: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        if let Some(leader) = self.running_container_leader().await {
            if self.leader_has_keepalive_marker(&leader) {
                return Ok(leader);
            }

            if env_var_bool(ALLOW_TRANSIENT_CONTAINER_NSENTER_ENV, false) {
                tracing::warn!(
                    workspace = %self.workspace.name,
                    leader = %leader,
                    "Attaching to transient container leader because override is enabled"
                );
                return Ok(leader);
            }

            anyhow::bail!(
                "Container workspace '{}' is currently led by transient process {}. Refusing to attach a long-running CLI to it because it can be SIGKILLed when that leader exits.",
                self.workspace.name,
                leader
            );
        }

        self.start_persistent_container_leader(env).await?;
        for attempt in 1..=50 {
            if let Some(leader) = self.running_container_leader().await {
                if self.leader_has_keepalive_marker(&leader) {
                    return Ok(leader);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100 * attempt.min(5))).await;
        }

        anyhow::bail!(
            "Persistent container leader for workspace '{}' did not become ready",
            self.workspace.name
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_nsenter_command(
        &self,
        leader: &str,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
        tailscale_bootstrap: bool,
        tailnet_only: bool,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
        scope_unit: Option<&str>,
    ) -> anyhow::Result<Command> {
        let nsenter = if Path::new("/usr/bin/nsenter").exists() {
            "/usr/bin/nsenter"
        } else {
            "nsenter"
        };
        let rel_cwd = self.rel_path_in_container(cwd);
        // nsenter preserves its caller's environment. Pass workspace variables
        // through the process environment instead of embedding them in the
        // shell argument: transient scope descriptions and process listings
        // expose argv, and workspace variables commonly contain credentials.
        let env: HashMap<_, _> = env
            .into_iter()
            .filter(|(key, _)| {
                let valid = Self::valid_env_key(key);
                if !valid {
                    tracing::warn!(key = %key, "Skipping env var with invalid key characters");
                }
                valid
            })
            .collect();
        let empty_env = HashMap::new();
        let shell_cmd = if tailscale_bootstrap {
            tracing::info!(
                workspace = %self.workspace.name,
                tailnet_only = %tailnet_only,
                "WorkspaceExec: nsenter with Tailscale bootstrap"
            );
            Self::build_tailscale_bootstrap_command(
                &rel_cwd,
                program,
                args,
                &empty_env,
                true,
                tailnet_only,
            )
        } else {
            Self::build_shell_command_with_env(&rel_cwd, program, args, None)
        };
        // Per-mission isolation, nsenter edition. `nsenter` only enters the
        // container's *namespaces* — the spawned process stays in the
        // caller's cgroup (the API service's), so without this wrapper a
        // runaway build attached to an already-running container bypasses
        // the boot-path mission scope entirely (observed live: a 47 GiB
        // Lean build throttling the whole service cgroup while the
        // container's own scope sat at 24G/2MB). Wrap each attach in its
        // own capped transient scope when `MISSION_MEMORY_MAX` is set.
        // The unit embeds the machine name so the API layer can find and
        // retune every scope belonging to one workspace at runtime, plus the
        // mission tag so mission-end teardown can stop exactly this
        // mission's scopes.
        let caps = self.mission_resource_caps();
        let mission_id = env
            .get("MISSION_ID")
            .and_then(|value| uuid::Uuid::parse_str(value).ok());
        let exec_unit = scope_unit.map(str::to_owned).unwrap_or_else(|| {
            exec_scope_unit_for_mission(
                &self.machine_name().unwrap_or_else(|| "unknown".to_string()),
                Some(cwd),
                mission_id,
            )
        });
        let mut cmd = if let Some(scope_args) = caps.scope_run_args(&exec_unit) {
            let mut c = Command::new("systemd-run");
            c.args(&scope_args);
            c.arg(nsenter);
            c
        } else {
            Command::new(nsenter)
        };
        cmd.args([
            "--target", leader, "--mount", "--uts", "--ipc", "--net", "--pid",
        ]);
        // `--root` is what stops an attached CLI from writing to the *host*
        // filesystem. Without it, nsenter enters the container's mount NS
        // but the new process keeps the caller's (= host's) root directory,
        // so resolves `/etc/...` and `/usr/local/bin/...` against the host
        // even though mounts are container-local. That's the path a
        // goal-mode mission used to drop `sandboxed-sh-prod-hotswap-*`
        // binaries into the host's `/usr/local/bin` and rewrite the host's
        // nginx config.
        //
        // Gated by an env var while we're confirming every container rootfs
        // has the harness tools (`claude`, `codex`, `opencode`, MCP binaries)
        // installed inside it. Default = legacy unsafe behaviour so an
        // existing deployment doesn't break on upgrade; once a host is
        // verified, set `SANDBOXED_SH_NSENTER_USE_TARGET_ROOT=1` in the env
        // file to flip the safe default on.
        let use_target_root = env_var_bool(NSENTER_USE_TARGET_ROOT_ENV, false);
        if use_target_root {
            // `--root` with no arg = use the *target* process's root, i.e.
            // the container rootfs. After this flag the new shell can only
            // see the container's `/etc`, `/usr/local/bin`, etc.
            cmd.arg("--root");
        }
        cmd.args(["/bin/sh", "-lc"]);
        cmd.arg(shell_cmd);
        cmd.env_clear().envs(env);
        cmd.stdin(stdin).stdout(stdout).stderr(stderr);
        Ok(cmd)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_command(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
        scope_unit: Option<&str>,
    ) -> anyhow::Result<Command> {
        match self.workspace.workspace_type {
            WorkspaceType::Host => {
                // For Host workspaces, spawn the command directly with environment variables.
                // We pass env vars directly via Command::envs() rather than shell export
                // to avoid issues with shell profile sourcing that can cause timeouts.
                let mut cmd = Command::new(program);
                cmd.current_dir(cwd);
                if !args.is_empty() {
                    cmd.args(args);
                }
                if !env.is_empty() {
                    cmd.envs(env);
                }
                cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                Ok(cmd)
            }
            WorkspaceType::Container => {
                if !use_nspawn_for_workspace(&self.workspace) {
                    // Fallback: execute on host when systemd-nspawn isn't available.
                    let mut cmd = Command::new(program);
                    cmd.current_dir(cwd);
                    if !args.is_empty() {
                        cmd.args(args);
                    }
                    if !env.is_empty() {
                        cmd.envs(env);
                    }
                    cmd.stdin(stdin).stdout(stdout).stderr(stderr);
                    return Ok(cmd);
                }

                let mut env = env;
                if !env.contains_key("HOME") {
                    env.insert("HOME".to_string(), "/root".to_string());
                }

                let fido_agent_path = Path::new("/run/sandboxed-sh/fido-agent.sock");
                if fido_agent_path.exists() {
                    env.insert(
                        "SSH_AUTH_SOCK".to_string(),
                        "/run/sandboxed-sh/fido-agent.sock".to_string(),
                    );
                }

                // Debug: log env vars relevant to Tailscale
                let has_ts_authkey = env.contains_key("TS_AUTHKEY");
                let has_ts_exit_node = env.contains_key("TS_EXIT_NODE");
                tracing::debug!(
                    workspace = %self.workspace.name,
                    has_ts_authkey = %has_ts_authkey,
                    has_ts_exit_node = %has_ts_exit_node,
                    env_keys = ?env.keys().collect::<Vec<_>>(),
                    "WorkspaceExec: checking Tailscale env vars"
                );

                // Determine if Tailscale bootstrap is needed before the nsenter
                // check, so the nsenter path can also include the bootstrap.
                let tailscale_enabled_check = nspawn::tailscale_enabled(&env);
                let tailscale_args = nspawn::tailscale_nspawn_extra_args(&env);
                let needs_tailscale_bootstrap =
                    tailscale_enabled_check && !tailscale_args.is_empty();

                tracing::info!(
                    workspace = %self.workspace.name,
                    tailscale_enabled_check = %tailscale_enabled_check,
                    tailscale_args_count = tailscale_args.len(),
                    needs_tailscale_bootstrap = %needs_tailscale_bootstrap,
                    "WorkspaceExec: Tailscale bootstrap decision"
                );
                // Calculate tailnet_only for nsenter path: TailnetOnly mode means
                // we connect to tailnet but use host gateway for internet.
                let nsenter_tailnet_only = needs_tailscale_bootstrap
                    && self
                        .workspace
                        .tailscale_mode
                        .unwrap_or(TailscaleMode::ExitNode)
                        == TailscaleMode::TailnetOnly;
                let leader = self.ensure_persistent_container_leader(&env).await?;
                self.build_nsenter_command(
                    &leader,
                    cwd,
                    program,
                    args,
                    env,
                    needs_tailscale_bootstrap,
                    nsenter_tailnet_only,
                    stdin,
                    stdout,
                    stderr,
                    scope_unit,
                )
            }
        }
    }

    pub async fn output(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
    ) -> anyhow::Result<std::process::Output> {
        // Retry loop: when multiple missions target the same container workspace,
        // concurrent systemd-nspawn boots race for a directory lock. The loser gets
        // "Directory tree … is currently busy".  On retry build_command() re-checks
        // running_container_leader() and will find the now-registered leader, falling
        // back to nsenter (which is concurrent-safe).
        let max_attempts: u64 = if self.workspace.workspace_type == WorkspaceType::Container {
            8
        } else {
            1
        };

        for attempt in 1..=max_attempts {
            let env_for_attempt = self.build_env(env.clone());
            let mut cmd = self
                .build_command(
                    cwd,
                    program,
                    args,
                    env_for_attempt,
                    Stdio::null(),
                    Stdio::piped(),
                    Stdio::piped(),
                    None,
                )
                .await
                .context("Failed to build workspace command")?;
            let output = cmd
                .output()
                .await
                .context("Failed to run workspace command")?;

            // Detect nspawn container boot race and retry.
            // Two failure modes when multiple missions race to boot the same container:
            //   1. "Directory tree … is currently busy" — nspawn rejects the boot
            //   2. Process killed with signal 9 + empty output — nspawn loses the lock race
            // In both cases, retrying lets build_command() find the now-running container
            // leader and fall back to nsenter.
            if !output.status.success() && attempt < max_attempts {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
                let is_busy = combined.contains("currently busy");
                let is_killed = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        output.status.signal() == Some(9) && combined.trim().is_empty()
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                };
                if is_busy || is_killed {
                    tracing::info!(
                        workspace = %self.workspace.name,
                        attempt = attempt,
                        max_attempts = max_attempts,
                        is_busy = is_busy,
                        is_killed = is_killed,
                        "Container nspawn race detected, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
                    continue;
                }
            }

            return Ok(output);
        }
        unreachable!()
    }

    pub async fn spawn_streaming(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
    ) -> anyhow::Result<Child> {
        let env = self.build_env(env);
        let mut cmd = self
            .build_command(
                cwd,
                program,
                args,
                env,
                Stdio::piped(), // Pipe stdin for processes that read input (e.g., Claude Code --print)
                Stdio::piped(),
                Stdio::piped(),
                None,
            )
            .await
            .context("Failed to build workspace command")?;

        let child = cmd.spawn().context("Failed to spawn workspace command")?;
        Ok(child)
    }

    /// Spawn a workspace-aware command with caller-provided stdio.
    ///
    /// Durable jobs use file-backed stdout/stderr rather than pipes so they
    /// can outlive the agent turn that launched them without blocking on an
    /// abandoned pipe reader.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_with_stdio(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
        durable_job_id: uuid::Uuid,
    ) -> anyhow::Result<Child> {
        let env = self.build_env(env);
        let host_env =
            matches!(self.workspace.workspace_type, WorkspaceType::Host).then(|| env.clone());
        let scope_unit = self
            .machine_name()
            .map(|machine| durable_scope_unit(&machine, durable_job_id));
        let mut cmd = self
            .build_command(
                cwd,
                program,
                args,
                env,
                stdin,
                stdout,
                stderr,
                scope_unit.as_deref(),
            )
            .await
            .context("Failed to build workspace command")?;
        // Host durable jobs execute in the API host namespace, so unlike an
        // nspawn/nsenter command there is no isolation boundary that drops
        // the service process environment. Replace it explicitly with the
        // workspace/job allowlist before spawning; otherwise a workspace
        // owner could read API credentials through a durable job's logs.
        if let Some(host_env) = host_env {
            replace_command_env(&mut cmd, host_env);
        }
        // Durable-job cancellation targets the spawned process group. Give
        // every workspace-aware launch its own session so cancelling a job
        // cannot signal the API service's process group. Descendants of a
        // systemd-run/nsenter wrapper inherit this group as well.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().context("Failed to spawn workspace command")
    }

    pub async fn spawn_streaming_pty(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: HashMap<String, String>,
    ) -> anyhow::Result<PtyChild> {
        let mut env = self.build_env(env);
        // A number of CLIs (notably Claude Code) behave differently without TERM.
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());

        // On Unix, use raw openpty() for Host workspaces and for Container
        // workspaces that go through nspawn/nsenter. portable-pty 0.9's
        // spawn_command resets signal dispositions and sweeps random fds in
        // pre_exec, which makes Claude Code CLI hang producing no PTY output.
        // Raw openpty with a minimal `setsid`/`TIOCSCTTY` pre_exec works.
        #[cfg(unix)]
        if matches!(self.workspace.workspace_type, WorkspaceType::Host) {
            return self.spawn_unix_pty(cwd, program, args, &env);
        }

        #[cfg(unix)]
        if matches!(self.workspace.workspace_type, WorkspaceType::Container)
            && use_nspawn_for_workspace(&self.workspace)
        {
            let (nsenter_program, nsenter_args) = self
                .build_container_nsenter_invocation(cwd, program, args, &mut env)
                .await?;
            return self.spawn_unix_pty(cwd, &nsenter_program, &nsenter_args, &env);
        }

        // Portable-pty fallback (non-Unix, or non-nspawn Container).
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to open PTY")?;

        let cmd = match self.workspace.workspace_type {
            WorkspaceType::Host => {
                // Non-Unix fallback (portable-pty)
                let mut cmd = CommandBuilder::new(program);
                cmd.cwd(cwd);
                if !args.is_empty() {
                    cmd.args(args);
                }
                for (k, v) in &env {
                    if k.trim().is_empty() {
                        continue;
                    }
                    cmd.env(k, v);
                }
                cmd
            }
            WorkspaceType::Container => {
                if !use_nspawn_for_workspace(&self.workspace) {
                    let mut cmd = CommandBuilder::new(program);
                    cmd.cwd(cwd);
                    if !args.is_empty() {
                        cmd.args(args);
                    }
                    for (k, v) in &env {
                        if k.trim().is_empty() {
                            continue;
                        }
                        cmd.env(k, v);
                    }
                    cmd
                } else {
                    // On Unix this branch is handled by `spawn_unix_pty` above;
                    // build the same nsenter invocation here for non-Unix.
                    let (nsenter_program, nsenter_args) = self
                        .build_container_nsenter_invocation(cwd, program, args, &mut env)
                        .await?;
                    let mut cmd = CommandBuilder::new(&nsenter_program);
                    for arg in &nsenter_args {
                        cmd.arg(arg);
                    }
                    cmd
                }
            }
        };

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn PTY command")?;
        // Drop the slave so the child owns the TTY; we only keep the master side.
        drop(pair.slave);

        Ok(PtyChild {
            child: PtyChildProcess::PortablePty(child),
            master: PtyMasterHandle::PortablePty(pair.master),
        })
    }

    /// Build the `(program, args)` for an *interactive* shell that joins this
    /// workspace's shared persistent container leader via `nsenter` — the same
    /// mechanism mission harnesses use (see [`Self::spawn_streaming_pty`]).
    ///
    /// The dashboard workspace shell uses this so it no longer boots a second
    /// `systemd-nspawn -D <dir>` on a directory a running mission already holds,
    /// which nspawn rejects with "Directory tree … is currently busy".
    /// Joining the existing leader via `nsenter` is concurrent-safe.
    ///
    /// Returns `Ok(None)` for workspaces that don't go through nspawn (host, or
    /// the non-nspawn container fallback); the caller should spawn the shell
    /// directly in that case.
    pub async fn build_interactive_shell_invocation(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        extra_env: HashMap<String, String>,
    ) -> anyhow::Result<Option<(String, Vec<String>)>> {
        if !(self.workspace.workspace_type == WorkspaceType::Container
            && use_nspawn_for_workspace(&self.workspace))
        {
            return Ok(None);
        }
        let mut env = self.build_env(extra_env);
        // Mirror spawn_streaming_pty: interactive shells (and some CLIs) behave
        // oddly without TERM, and nsenter does not propagate the caller's env.
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());
        let invocation = self
            .build_container_nsenter_invocation(cwd, program, args, &mut env)
            .await?;
        Ok(Some(invocation))
    }

    /// Build the (program, args) tuple for spawning a command inside an
    /// nspawn container via nsenter. Also mutates `env` to add container
    /// defaults (HOME, SSH_AUTH_SOCK). The returned args end with
    /// `"/bin/sh", "-lc", <shell_cmd>`. nsenter inherits the environment
    /// applied to the returned command by [`Self::spawn_unix_pty`].
    async fn build_container_nsenter_invocation(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: &mut HashMap<String, String>,
    ) -> anyhow::Result<(String, Vec<String>)> {
        if !env.contains_key("HOME") {
            env.insert("HOME".to_string(), "/root".to_string());
        }

        let fido_agent_path = Path::new("/run/sandboxed-sh/fido-agent.sock");
        if fido_agent_path.exists() {
            env.insert(
                "SSH_AUTH_SOCK".to_string(),
                "/run/sandboxed-sh/fido-agent.sock".to_string(),
            );
        }

        let tailscale_enabled_check = nspawn::tailscale_enabled(env);
        let tailscale_args = nspawn::tailscale_nspawn_extra_args(env);
        let needs_tailscale_bootstrap = tailscale_enabled_check && !tailscale_args.is_empty();
        let nsenter_tailnet_only = needs_tailscale_bootstrap
            && self
                .workspace
                .tailscale_mode
                .unwrap_or(TailscaleMode::ExitNode)
                == TailscaleMode::TailnetOnly;

        let leader = self.ensure_persistent_container_leader(env).await?;
        let nsenter = if Path::new("/usr/bin/nsenter").exists() {
            "/usr/bin/nsenter"
        } else {
            "nsenter"
        }
        .to_string();
        let rel_cwd = self.rel_path_in_container(cwd);
        let empty_env = HashMap::new();
        let shell_cmd = if needs_tailscale_bootstrap {
            Self::build_tailscale_bootstrap_command(
                &rel_cwd,
                program,
                args,
                &empty_env,
                true,
                nsenter_tailnet_only,
            )
        } else {
            Self::build_shell_command_with_env(&rel_cwd, program, args, None)
        };

        let mut nsenter_args = vec![
            "--target".to_string(),
            leader,
            "--mount".to_string(),
            "--uts".to_string(),
            "--ipc".to_string(),
            "--net".to_string(),
            "--pid".to_string(),
        ];
        // Keep PTY launches under the same target-root guard as non-PTY
        // nsenter. Entering only the mount namespace still retains the host
        // root directory, allowing absolute paths to escape the container.
        append_nsenter_target_root_arg(
            &mut nsenter_args,
            env_var_bool(NSENTER_USE_TARGET_ROOT_ENV, false),
        );
        nsenter_args.extend(["/bin/sh".to_string(), "-lc".to_string(), shell_cmd]);

        // Same cgroup-escape hatch as build_nsenter_command, PTY edition:
        // this invocation is what launches harness CLIs (claude/codex/…) for
        // missions, and without the scope wrapper the harness — and every
        // build it spawns — lands in the API service's cgroup. That is
        // exactly how a 45 GiB `lean` run throttled the whole prod service
        // on 2026-06-07 despite boot-path caps being deployed. systemd-run
        // --scope keeps the payload as its own foreground child, so PTY
        // semantics and group-kill teardown are preserved.
        let caps = self.mission_resource_caps();
        let mission_id = env
            .get("MISSION_ID")
            .and_then(|value| uuid::Uuid::parse_str(value).ok());
        let exec_unit = exec_scope_unit_for_mission(
            &self.machine_name().unwrap_or_else(|| "unknown".to_string()),
            Some(cwd),
            mission_id,
        );
        if let Some(scope_args) = caps.scope_run_args(&exec_unit) {
            let mut args = scope_args;
            args.push(nsenter);
            args.extend(nsenter_args);
            return Ok(("systemd-run".to_string(), args));
        }

        Ok((nsenter, nsenter_args))
    }

    /// Spawn a process in a raw Unix PTY. Used for Host workspaces and for
    /// nspawn Container workspaces (with `program`/`args` pre-wrapped in
    /// `nsenter ... /bin/sh -lc ...` by
    /// [`Self::build_container_nsenter_invocation`]).
    #[cfg(unix)]
    fn spawn_unix_pty(
        &self,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> anyhow::Result<PtyChild> {
        use std::os::unix::io::{FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;

        let mut master_raw: libc::c_int = 0;
        let mut slave_raw: libc::c_int = 0;

        // SAFETY: master_raw and slave_raw are valid mutable pointers;
        // remaining args are null (no name buffer, no termios, no winsize).
        let ret = unsafe {
            libc::openpty(
                &mut master_raw,
                &mut slave_raw,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ret != 0 {
            anyhow::bail!("openpty() failed: {}", std::io::Error::last_os_error());
        }

        // Set terminal size (24x80).
        let ws = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master_raw is a valid PTY fd from openpty() above;
        // &ws is a valid pointer to a properly initialized winsize struct.
        unsafe {
            libc::ioctl(master_raw, libc::TIOCSWINSZ, &ws);
        }

        let mut cmd = std::process::Command::new(program);
        cmd.current_dir(cwd);
        if !args.is_empty() {
            cmd.args(args);
        }
        // Container commands must not inherit API-service credentials. The
        // complete workspace environment is applied here and inherited by
        // nsenter without ever appearing in its argv.
        if self.workspace.workspace_type == WorkspaceType::Container {
            cmd.env_clear();
        }
        for (k, v) in env {
            if !Self::valid_env_key(k) {
                continue;
            }
            cmd.env(k, v);
        }

        // Wire the PTY slave as stdin/stdout/stderr.
        // SAFETY: slave_raw is a valid fd from openpty(). We dup() it three
        // times and transfer ownership of each duplicate to Stdio via
        // from_raw_fd(). The dup return values are checked for errors.
        // pre_exec runs between fork() and exec() in the child process,
        // where only async-signal-safe functions are called (close, setsid,
        // ioctl are all async-signal-safe).
        unsafe {
            let slave_in = libc::dup(slave_raw);
            let slave_out = libc::dup(slave_raw);
            let slave_err = libc::dup(slave_raw);
            if slave_in < 0 || slave_out < 0 || slave_err < 0 {
                libc::close(master_raw);
                libc::close(slave_raw);
                if slave_in >= 0 {
                    libc::close(slave_in);
                }
                if slave_out >= 0 {
                    libc::close(slave_out);
                }
                if slave_err >= 0 {
                    libc::close(slave_err);
                }
                anyhow::bail!(
                    "dup() for PTY slave failed: {}",
                    std::io::Error::last_os_error()
                );
            }

            cmd.stdin(std::process::Stdio::from_raw_fd(slave_in));
            cmd.stdout(std::process::Stdio::from_raw_fd(slave_out));
            cmd.stderr(std::process::Stdio::from_raw_fd(slave_err));

            let m = master_raw;
            let s = slave_raw;
            cmd.pre_exec(move || {
                // Close inherited parent-side fds.
                libc::close(m);
                libc::close(s);
                // New session so the child gets its own process group.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Set the PTY slave (now fd 0) as the controlling terminal.
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0 as libc::c_int) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = cmd.spawn().context("Failed to spawn Host PTY command")?;

        // Close slave in parent - child owns it now.
        // SAFETY: slave_raw is a valid fd; after close the child holds the
        // only remaining references via the duped stdin/stdout/stderr.
        unsafe {
            libc::close(slave_raw);
        }

        // SAFETY: master_raw is a valid fd from openpty() and we transfer
        // sole ownership to OwnedFd (no other code will close it).
        let master_fd = unsafe { OwnedFd::from_raw_fd(master_raw) };

        Ok(PtyChild {
            child: PtyChildProcess::Std(child),
            master: PtyMasterHandle::Unix(master_fd),
        })
    }
}
