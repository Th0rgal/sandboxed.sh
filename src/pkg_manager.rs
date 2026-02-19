//! Unified package manager helpers — bun-first, npm-fallback.
//!
//! Every call-site that needs to install, uninstall, or run a global JS package
//! should go through these helpers so the strategy is defined in one place.

use tokio::process::Command;

/// Which JS package manager / runner is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgManager {
    Bun,
    Npm,
}

impl PkgManager {
    /// The binary name used for *installing* packages (`bun` or `npm`).
    pub fn bin(&self) -> &'static str {
        match self {
            PkgManager::Bun => "bun",
            PkgManager::Npm => "npm",
        }
    }

    /// The binary name used for *running* packages (`bunx` or `npx`).
    pub fn runner(&self) -> &'static str {
        match self {
            PkgManager::Bun => "bunx",
            PkgManager::Npm => "npx",
        }
    }

    /// Returns the arguments for a global install, e.g. `["install", "-g", pkg]`
    /// for npm or `["install", "-g", pkg]` for bun (same shape).
    pub fn global_install_args(&self, package: &str) -> Vec<String> {
        vec![
            "install".to_string(),
            "-g".to_string(),
            package.to_string(),
        ]
    }

    /// Returns the arguments for a global uninstall.
    pub fn global_uninstall_args(&self, package: &str) -> Vec<String> {
        match self {
            PkgManager::Bun => vec![
                "remove".to_string(),
                "-g".to_string(),
                package.to_string(),
            ],
            PkgManager::Npm => vec![
                "uninstall".to_string(),
                "-g".to_string(),
                package.to_string(),
            ],
        }
    }

    /// Returns the shell one-liner for a global install (useful in generated scripts).
    pub fn global_install_cmd(&self, package: &str) -> String {
        match self {
            PkgManager::Bun => format!("bun install -g {package}"),
            PkgManager::Npm => format!("npm install -g {package}"),
        }
    }
}

/// Detect whether `bun` is available on the **host** system.
/// Checks both `PATH` and the well-known `/root/.bun/bin/bun` location.
pub async fn bun_available() -> bool {
    let in_path = Command::new("bun")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    if in_path {
        return true;
    }
    Command::new("/root/.bun/bin/bun")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// Detect whether `npm` is available on the host system.
pub async fn npm_available() -> bool {
    Command::new("npm")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// Return the preferred package manager: **bun** if available, else **npm**.
pub async fn preferred() -> Option<PkgManager> {
    if bun_available().await {
        Some(PkgManager::Bun)
    } else if npm_available().await {
        Some(PkgManager::Npm)
    } else {
        None
    }
}

/// Generate a shell snippet that picks bun or npm at runtime.
/// Useful for scripts that run inside containers where we don't know at
/// generation time which package manager is available.
///
/// Example output:
/// ```bash
/// if command -v bun >/dev/null 2>&1; then
///   bun install -g @anthropic-ai/claude-code@latest
/// elif command -v npm >/dev/null 2>&1; then
///   npm install -g @anthropic-ai/claude-code@latest
/// else
///   echo "[sandboxed] No package manager found; skipping install"
/// fi
/// ```
pub fn shell_install_global(package: &str) -> String {
    format!(
        r#"if command -v bun >/dev/null 2>&1; then
  bun install -g {package}
elif command -v npm >/dev/null 2>&1; then
  npm install -g {package}
else
  echo "[sandboxed] No package manager (bun/npm) found; skipping {package} install"
fi"#,
        package = package
    )
}

/// Generate a shell snippet for running a package via bunx/npx.
pub fn shell_run_package(package_and_args: &str) -> String {
    format!(
        r#"if command -v bunx >/dev/null 2>&1; then
  bunx {package_and_args}
elif command -v npx >/dev/null 2>&1; then
  npx {package_and_args}
else
  echo "[sandboxed] No package runner (bunx/npx) found; skipping"
fi"#,
        package_and_args = package_and_args
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bun_install_args() {
        let pm = PkgManager::Bun;
        assert_eq!(
            pm.global_install_args("@anthropic-ai/claude-code@latest"),
            vec!["install", "-g", "@anthropic-ai/claude-code@latest"]
        );
    }

    #[test]
    fn npm_uninstall_args() {
        let pm = PkgManager::Npm;
        assert_eq!(
            pm.global_uninstall_args("@openai/codex"),
            vec!["uninstall", "-g", "@openai/codex"]
        );
    }

    #[test]
    fn bun_uninstall_args() {
        let pm = PkgManager::Bun;
        assert_eq!(
            pm.global_uninstall_args("@openai/codex"),
            vec!["remove", "-g", "@openai/codex"]
        );
    }

    #[test]
    fn runner_names() {
        assert_eq!(PkgManager::Bun.runner(), "bunx");
        assert_eq!(PkgManager::Npm.runner(), "npx");
    }

    #[test]
    fn shell_snippets_contain_both_managers() {
        let snippet = shell_install_global("foo@latest");
        assert!(snippet.contains("bun install -g foo@latest"));
        assert!(snippet.contains("npm install -g foo@latest"));
    }
}
