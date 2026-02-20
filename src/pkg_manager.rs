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

    /// Returns the arguments for a global install, e.g. `["install", "-g", pkg]`
    /// for npm or `["install", "-g", pkg]` for bun (same shape).
    pub fn global_install_args(&self, package: &str) -> Vec<String> {
        vec!["install".to_string(), "-g".to_string(), package.to_string()]
    }

    /// Returns the arguments for a global uninstall.
    pub fn global_uninstall_args(&self, package: &str) -> Vec<String> {
        match self {
            PkgManager::Bun => vec!["remove".to_string(), "-g".to_string(), package.to_string()],
            PkgManager::Npm => vec![
                "uninstall".to_string(),
                "-g".to_string(),
                package.to_string(),
            ],
        }
    }
}

/// Detect whether `bun` is available on the **host** system.
/// This intentionally checks only PATH because callers execute `bun` by binary
/// name via `Command::new("bun")`.
pub async fn bun_available() -> bool {
    Command::new("bun")
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
}
