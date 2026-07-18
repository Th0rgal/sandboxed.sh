//! Bridge error taxonomy with truthful OpenAI-compatible classification.
//!
//! The class drives both the HTTP status and the OpenAI `error.type`/`code`
//! fields. We keep provider vs infrastructure vs caller-fault distinct so
//! provenance stays honest: a Grok CLI crash is *infrastructure*, a rejected
//! caller request is *invalid_request*, and a route that is not provisioned is
//! *provider_configuration_error* — never dressed up as a model refusal.

use axum::http::StatusCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeErrorClass {
    /// Caller sent something malformed or inconsistent (400).
    InvalidRequest,
    /// The route exists but is not configured/healthy for use (503).
    ProviderConfiguration,
    /// The connected Grok backend or bridge transport failed (502).
    UpstreamInfrastructure,
    /// A prior session was cancelled, timed out, or expired (409).
    SessionState,
}

#[derive(Debug, Clone)]
pub struct BridgeError {
    pub class: BridgeErrorClass,
    pub message: String,
}

impl BridgeError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            class: BridgeErrorClass::InvalidRequest,
            message: message.into(),
        }
    }

    pub fn provider_configuration(message: impl Into<String>) -> Self {
        Self {
            class: BridgeErrorClass::ProviderConfiguration,
            message: message.into(),
        }
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self {
            class: BridgeErrorClass::UpstreamInfrastructure,
            message: message.into(),
        }
    }

    pub fn session_state(message: impl Into<String>) -> Self {
        Self {
            class: BridgeErrorClass::SessionState,
            message: message.into(),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self.class {
            BridgeErrorClass::InvalidRequest => StatusCode::BAD_REQUEST,
            BridgeErrorClass::ProviderConfiguration => StatusCode::SERVICE_UNAVAILABLE,
            BridgeErrorClass::UpstreamInfrastructure => StatusCode::BAD_GATEWAY,
            BridgeErrorClass::SessionState => StatusCode::CONFLICT,
        }
    }

    /// OpenAI `error.type`.
    pub fn openai_type(&self) -> &'static str {
        match self.class {
            BridgeErrorClass::InvalidRequest => "invalid_request_error",
            BridgeErrorClass::ProviderConfiguration => "provider_configuration_error",
            BridgeErrorClass::UpstreamInfrastructure => "upstream_error",
            BridgeErrorClass::SessionState => "session_state_error",
        }
    }

    /// OpenAI `error.code`.
    pub fn openai_code(&self) -> &'static str {
        match self.class {
            BridgeErrorClass::InvalidRequest => "invalid_request",
            BridgeErrorClass::ProviderConfiguration => "provider_configuration_error",
            BridgeErrorClass::UpstreamInfrastructure => "upstream_infrastructure_error",
            BridgeErrorClass::SessionState => "session_unavailable",
        }
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BridgeError {}
