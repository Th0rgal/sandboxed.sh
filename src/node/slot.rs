//! Optional external slot provider for a node.
//!
//! A node normally owns its own admission (`SANDBOXED_NODE_CAPACITY`). On a
//! box that also hosts inference or a CI runner, something else has to make
//! room before a build starts: on the DGX Spark that is the arbiter, which
//! stops vLLM, pauses the GitHub runner and gates on free memory. With
//! `SANDBOXED_NODE_SLOT_PROVIDER=arbiter` the runner asks the arbiter for a
//! slot before executing a job and releases it afterwards. The job itself
//! runs on the node as usual; the arbiter never runs commands for us.
//!
//! Failure policy: a provider that is unreachable is *not* a reason to run the
//! job anyway — that is exactly the situation (memory full of a model) the
//! provider exists for. The job waits and retries until cancelled or until the
//! job's own timeout is spent.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RETRY_INTERVAL: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct SlotProvider {
    base_url: String,
    token: String,
    client: reqwest::Client,
    /// Priority forwarded to the provider (`P0` preempts CI, `P1` queues
    /// behind it).
    priority: String,
    /// Memory hint forwarded to the provider (e.g. `16G`).
    mem: Option<String>,
}

/// Held for the duration of a job; releases the slot on drop.
pub struct SlotLease {
    provider: SlotProvider,
    job_id: Uuid,
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        let provider = self.provider.clone();
        let job_id = self.job_id;
        tokio::spawn(async move {
            if let Err(error) = provider.release(job_id).await {
                tracing::warn!(%job_id, %error, "slot provider: release failed");
            }
        });
    }
}

impl SlotProvider {
    /// `SANDBOXED_NODE_SLOT_PROVIDER=arbiter` with `SANDBOXED_NODE_ARBITER_URL`
    /// and `SANDBOXED_NODE_ARBITER_TOKEN`; anything else → `None` (the node
    /// admits on its own).
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let provider = std::env::var("SANDBOXED_NODE_SLOT_PROVIDER").unwrap_or_default();
        match provider.trim().to_ascii_lowercase().as_str() {
            "" | "none" | "off" => Ok(None),
            "arbiter" => {
                let base_url = std::env::var("SANDBOXED_NODE_ARBITER_URL")
                    .ok()
                    .map(|url| url.trim().trim_end_matches('/').to_string())
                    .filter(|url| !url.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "SANDBOXED_NODE_ARBITER_URL is required with SLOT_PROVIDER=arbiter"
                        )
                    })?;
                let token = std::env::var("SANDBOXED_NODE_ARBITER_TOKEN")
                    .ok()
                    .map(|token| token.trim().to_string())
                    .filter(|token| !token.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "SANDBOXED_NODE_ARBITER_TOKEN is required with SLOT_PROVIDER=arbiter"
                        )
                    })?;
                let priority = std::env::var("SANDBOXED_NODE_ARBITER_PRIORITY")
                    .ok()
                    .map(|p| p.trim().to_ascii_uppercase())
                    .filter(|p| p == "P0" || p == "P1")
                    .unwrap_or_else(|| "P0".to_string());
                let mem = std::env::var("SANDBOXED_NODE_ARBITER_MEM")
                    .ok()
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty());
                Ok(Some(Self::new(base_url, token, priority, mem)))
            }
            other => Err(anyhow::anyhow!(
                "unknown SANDBOXED_NODE_SLOT_PROVIDER '{other}' (expected 'arbiter' or unset)"
            )),
        }
    }

    pub fn new(base_url: String, token: String, priority: String, mem: Option<String>) -> Self {
        Self {
            base_url,
            token,
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client"),
            priority,
            mem,
        }
    }

    /// Block until the provider grants a slot for `job_id`, or the token is
    /// cancelled. Idempotent on the provider side: re-asking for a slot we
    /// already hold is a grant.
    pub async fn acquire(
        self: &Arc<Self>,
        job_id: Uuid,
        token: &CancellationToken,
    ) -> anyhow::Result<SlotLease> {
        let mut attempts: u64 = 0;
        loop {
            if token.is_cancelled() {
                anyhow::bail!("cancelled while waiting for a slot");
            }
            let body = serde_json::json!({
                "id": job_id.to_string(),
                "priority": self.priority,
                "mem": self.mem,
            });
            match self
                .client
                .post(format!("{}/slot/acquire", self.base_url))
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    tracing::info!(%job_id, attempts, "slot provider: slot granted");
                    return Ok(SlotLease {
                        provider: (**self).clone(),
                        job_id,
                    });
                }
                Ok(response) if response.status().as_u16() == 503 => {
                    if attempts.is_multiple_of(12) {
                        let reason = response.text().await.unwrap_or_default();
                        tracing::info!(%job_id, attempts, reason, "slot provider: busy, waiting");
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    anyhow::bail!("slot provider refused: {status} {text}");
                }
                Err(error) => {
                    if attempts.is_multiple_of(12) {
                        tracing::warn!(%job_id, %error, "slot provider unreachable; waiting");
                    }
                }
            }
            attempts += 1;
            tokio::select! {
                _ = token.cancelled() => anyhow::bail!("cancelled while waiting for a slot"),
                _ = tokio::time::sleep(RETRY_INTERVAL) => {}
            }
        }
    }

    pub async fn release(&self, job_id: Uuid) -> anyhow::Result<()> {
        let response = self
            .client
            .post(format!("{}/slot/release", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "id": job_id.to_string() }))
            .send()
            .await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            Ok(())
        } else {
            anyhow::bail!("slot release: {}", response.status())
        }
    }
}
