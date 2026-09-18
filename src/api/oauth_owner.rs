//! Single-owner policy for provider OAuth credentials.
//!
//! Anthropic and OpenAI OAuth refresh tokens rotate: every refresh returns a
//! new refresh token and revokes the previous one. When two processes hold
//! copies of the same token, whichever refreshes first wins and the other is
//! left with `invalid_grant` forever. Historically sandboxed.sh held several
//! copies at once (its own store, the host CLI credential tiers, per-mission
//! copies) next to CLIProxyAPI's own auth directory, and the periodic
//! refresher raced all of them.
//!
//! This module decides who owns a provider's OAuth credential. When
//! CLIProxyAPI holds a credential for a provider, sandboxed.sh must never
//! refresh its own copy of that provider's tokens: harnesses and the inference
//! proxy route through CLIProxyAPI instead, and the proxy alone refreshes.
//!
//! `SANDBOXED_OAUTH_OWNER=legacy` restores the previous behaviour (sandboxed.sh
//! refreshes everything itself) as a rollback switch.

use crate::ai_providers::ProviderType;

/// Environment switch selecting the credential owner policy.
pub const OWNER_MODE_ENV: &str = "SANDBOXED_OAUTH_OWNER";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthOwnerMode {
    /// CLIProxyAPI owns every OAuth credential it holds a file for.
    CliProxy,
    /// sandboxed.sh refreshes its own copies (pre-refactor behaviour).
    Legacy,
}

pub(crate) fn parse_owner_mode(raw: Option<&str>, cli_proxy_disabled: bool) -> OAuthOwnerMode {
    if cli_proxy_disabled {
        return OAuthOwnerMode::Legacy;
    }
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("legacy") | Some("self") | Some("sandboxed") => OAuthOwnerMode::Legacy,
        _ => OAuthOwnerMode::CliProxy,
    }
}

/// Current owner policy from the environment.
pub fn owner_mode() -> OAuthOwnerMode {
    parse_owner_mode(
        std::env::var(OWNER_MODE_ENV).ok().as_deref(),
        crate::util::env_var_bool("CLAUDE_CODE_DISABLE_CLI_PROXY", false),
    )
}

/// Providers whose OAuth credentials CLIProxyAPI can own.
pub(crate) fn provider_is_cli_proxy_capable(provider: ProviderType) -> bool {
    matches!(
        provider,
        ProviderType::Anthropic | ProviderType::OpenAI | ProviderType::Xai
    )
}

/// Whether CLIProxyAPI currently holds a refreshable credential for `provider`.
///
/// "Refreshable" (access + refresh token present, not disabled) rather than
/// "fresh": a momentarily expired proxy file is still the proxy's to refresh,
/// and must not hand ownership back to sandboxed.sh.
fn cli_proxy_holds_credential(provider: ProviderType) -> bool {
    match provider {
        ProviderType::Anthropic => {
            crate::api::ai_providers::has_refreshable_cli_proxy_claude_account()
        }
        ProviderType::OpenAI => crate::api::ai_providers::has_refreshable_cli_proxy_codex_account(),
        ProviderType::Xai => crate::api::ai_providers::xai_cli_proxy_account_available(),
        _ => false,
    }
}

/// Pure decision: may sandboxed.sh refresh `provider`'s OAuth tokens itself?
pub(crate) fn should_refresh_provider(
    provider: ProviderType,
    mode: OAuthOwnerMode,
    cli_proxy_holds: bool,
) -> bool {
    match mode {
        OAuthOwnerMode::Legacy => true,
        OAuthOwnerMode::CliProxy => !(provider_is_cli_proxy_capable(provider) && cli_proxy_holds),
    }
}

/// True when CLIProxyAPI owns `provider`'s OAuth credential: sandboxed.sh must
/// not refresh, rotate or mint tokens for it.
pub fn cli_proxy_owns(provider: ProviderType) -> bool {
    !should_refresh_provider(provider, owner_mode(), cli_proxy_holds_credential(provider))
}

/// Where a harness reaches CLIProxyAPI and how it authenticates to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliProxyEndpoint {
    /// Base URL without a trailing slash, e.g. `http://127.0.0.1:8317`.
    pub base_url: String,
    /// Bearer key CLIProxyAPI expects (inert placeholder when it runs open).
    pub api_key: String,
}

impl CliProxyEndpoint {
    /// OpenAI-compatible root (`/v1`) used by Codex and OpenCode.
    pub fn openai_v1_url(&self) -> String {
        format!("{}/v1", self.base_url)
    }
}

/// The CLIProxyAPI endpoint from the environment (same aliases as the
/// inference proxy uses), or `None` when the proxy is disabled.
pub fn cli_proxy_endpoint() -> Option<CliProxyEndpoint> {
    if crate::util::env_var_bool("CLAUDE_CODE_DISABLE_CLI_PROXY", false) {
        return None;
    }
    let base_url = crate::util::cli_proxy_base_url_from_env()
        .unwrap_or_else(|| "http://127.0.0.1:8317".to_string())
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return None;
    }
    let api_key = crate::util::cli_proxy_api_key_from_env()
        .unwrap_or_else(|| "sandboxed-sh-cli-proxy".to_string());
    Some(CliProxyEndpoint { base_url, api_key })
}

/// Endpoint a harness must use for `provider` when CLIProxyAPI owns its OAuth
/// credential; `None` means the harness keeps its own credentials.
pub fn harness_via_cli_proxy(provider: ProviderType) -> Option<CliProxyEndpoint> {
    if cli_proxy_owns(provider) {
        cli_proxy_endpoint()
    } else {
        None
    }
}

/// Endpoint Codex must use when CLIProxyAPI owns the ChatGPT/Codex OAuth
/// credential; `None` means Codex keeps its own credentials.
pub fn codex_via_cli_proxy() -> Option<CliProxyEndpoint> {
    harness_via_cli_proxy(ProviderType::OpenAI)
}

/// Log-and-skip helper for refresh entry points.
pub(crate) fn skip_refresh_if_owned(provider: ProviderType, what: &str) -> bool {
    if cli_proxy_owns(provider) {
        tracing::debug!(
            provider = provider.id(),
            what,
            "Skipping OAuth refresh: credential is owned by CLIProxyAPI"
        );
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_mode_defaults_to_cli_proxy_and_honours_rollback() {
        assert_eq!(parse_owner_mode(None, false), OAuthOwnerMode::CliProxy);
        assert_eq!(
            parse_owner_mode(Some("cli-proxy"), false),
            OAuthOwnerMode::CliProxy
        );
        assert_eq!(
            parse_owner_mode(Some("legacy"), false),
            OAuthOwnerMode::Legacy
        );
        assert_eq!(
            parse_owner_mode(Some(" Legacy "), false),
            OAuthOwnerMode::Legacy
        );
        // Disabling the CLI proxy entirely means sandboxed.sh must keep refreshing.
        assert_eq!(parse_owner_mode(None, true), OAuthOwnerMode::Legacy);
    }

    #[test]
    fn refresh_decision_table() {
        use OAuthOwnerMode::*;
        // Legacy always refreshes.
        assert!(should_refresh_provider(
            ProviderType::Anthropic,
            Legacy,
            true
        ));
        assert!(should_refresh_provider(ProviderType::OpenAI, Legacy, true));
        // CLI proxy mode: owned providers with a proxy credential are skipped.
        assert!(!should_refresh_provider(
            ProviderType::Anthropic,
            CliProxy,
            true
        ));
        assert!(!should_refresh_provider(
            ProviderType::OpenAI,
            CliProxy,
            true
        ));
        assert!(!should_refresh_provider(ProviderType::Xai, CliProxy, true));
        // ...but still refreshed when the proxy holds nothing for them.
        assert!(should_refresh_provider(
            ProviderType::Anthropic,
            CliProxy,
            false
        ));
        // Providers the proxy cannot own are always sandboxed.sh's.
        assert!(should_refresh_provider(ProviderType::Kimi, CliProxy, true));
        assert!(should_refresh_provider(
            ProviderType::Google,
            CliProxy,
            true
        ));
    }
}
