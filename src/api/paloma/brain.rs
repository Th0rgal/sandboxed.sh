use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainProposal {
    pub label: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowBrainRun {
    pub deterministic: BrainProposal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<BrainProposal>,
    pub shadow_send_allowed: bool,
}

#[async_trait]
pub trait PalomaBrain: Send + Sync {
    async fn classify_user_message(&self, text: &str) -> BrainProposal;
    async fn extract_preference(&self, text: &str) -> Option<BrainProposal>;
    async fn draft_digest(&self, context: &str) -> BrainProposal;
    async fn summarize_mission(&self, context: &str) -> BrainProposal;
}

pub struct ShadowPalomaBrain<B> {
    inner: B,
    label: String,
}

impl<B> ShadowPalomaBrain<B> {
    pub fn new(inner: B, label: impl Into<String>) -> Self {
        Self {
            inner,
            label: label.into(),
        }
    }
}

#[async_trait]
impl<B> PalomaBrain for ShadowPalomaBrain<B>
where
    B: PalomaBrain,
{
    async fn classify_user_message(&self, text: &str) -> BrainProposal {
        let mut proposal = self.inner.classify_user_message(text).await;
        proposal.label = format!("{}_{}", self.label, proposal.label);
        proposal
    }

    async fn extract_preference(&self, text: &str) -> Option<BrainProposal> {
        self.inner
            .extract_preference(text)
            .await
            .map(|mut proposal| {
                proposal.label = format!("{}_{}", self.label, proposal.label);
                proposal
            })
    }

    async fn draft_digest(&self, context: &str) -> BrainProposal {
        let mut proposal = self.inner.draft_digest(context).await;
        proposal.label = format!("{}_{}", self.label, proposal.label);
        proposal
    }

    async fn summarize_mission(&self, context: &str) -> BrainProposal {
        let mut proposal = self.inner.summarize_mission(context).await;
        proposal.label = format!("{}_{}", self.label, proposal.label);
        proposal
    }
}

pub async fn compare_shadow_digest<D, S>(
    deterministic: &D,
    shadow: Option<&S>,
    context: &str,
) -> ShadowBrainRun
where
    D: PalomaBrain,
    S: PalomaBrain,
{
    ShadowBrainRun {
        deterministic: deterministic.draft_digest(context).await,
        shadow: match shadow {
            Some(shadow) => Some(shadow.draft_digest(context).await),
            None => None,
        },
        // LLM/QwenPaw-backed brains are always proposal-only. Delivery must go
        // through Paloma policy and decision logging.
        shadow_send_allowed: false,
    }
}

#[derive(Debug, Default)]
pub struct DeterministicPalomaBrain;

#[async_trait]
impl PalomaBrain for DeterministicPalomaBrain {
    async fn classify_user_message(&self, text: &str) -> BrainProposal {
        BrainProposal {
            label: if text.trim_start().starts_with('/') {
                "command"
            } else {
                "message"
            }
            .to_string(),
            text: text.to_string(),
        }
    }

    async fn extract_preference(&self, text: &str) -> Option<BrainProposal> {
        let lowered = text.to_ascii_lowercase();
        (lowered.contains("mute") || lowered.contains("only tell me")).then(|| BrainProposal {
            label: "preference".to_string(),
            text: text.to_string(),
        })
    }

    async fn draft_digest(&self, context: &str) -> BrainProposal {
        BrainProposal {
            label: "deterministic_digest".to_string(),
            text: context.to_string(),
        }
    }

    async fn summarize_mission(&self, context: &str) -> BrainProposal {
        BrainProposal {
            label: "deterministic_summary".to_string(),
            text: context.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shadow_brain_never_gets_direct_send_authority() {
        let deterministic = DeterministicPalomaBrain;
        let shadow = ShadowPalomaBrain::new(DeterministicPalomaBrain, "qwenpaw_shadow");

        let run = compare_shadow_digest(&deterministic, Some(&shadow), "mission update").await;

        assert_eq!(run.deterministic.label, "deterministic_digest");
        assert_eq!(
            run.shadow.expect("shadow proposal").label,
            "qwenpaw_shadow_deterministic_digest"
        );
        assert!(!run.shadow_send_allowed);
    }
}
