//! Core-side HTTP client for talking to `sandboxed-node` runners.

use std::time::Duration;

use uuid::Uuid;

use super::protocol::{
    CancelJobResponse, ExecuteResponse, LeaseRequest, NodeHeartbeat, NodeJobStatus,
    SubmitJobRequest, SubmitJobResponse,
};
use super::{RemoteNodeConfig, RemoteNodeError};

/// Total request timeout for `/execute`, which blocks until the remote
/// command completes.
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// `POST /jobs` returns as soon as the job is queued.
const SUBMIT_JOB_TIMEOUT: Duration = Duration::from_secs(30);

/// Job status/cancel calls are cheap lookups.
const JOB_STATUS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct RemoteNodeClient {
    http: reqwest::Client,
}

impl Default for RemoteNodeClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl RemoteNodeClient {
    pub async fn heartbeat(
        &self,
        node: &RemoteNodeConfig,
        token: &str,
    ) -> Result<NodeHeartbeat, RemoteNodeError> {
        let url = format!("{}/heartbeat", node.base_url);
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        if !response.status().is_success() {
            return Err(RemoteNodeError::Request(format!(
                "heartbeat returned {}",
                response.status()
            )));
        }
        response
            .json::<NodeHeartbeat>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }

    pub async fn execute(
        &self,
        node: &RemoteNodeConfig,
        shared_token: &str,
        request: &LeaseRequest,
    ) -> Result<ExecuteResponse, RemoteNodeError> {
        let url = format!("{}/execute", node.base_url);
        let response = self
            .http
            .post(url)
            // Override the client-wide 30s timeout: remote commands
            // (builds, tests) routinely run for minutes and the response
            // only arrives once the command finishes.
            .timeout(EXECUTE_TIMEOUT)
            .bearer_auth(shared_token)
            .json(request)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteNodeError::Rejected {
                status: status.as_u16(),
                body,
            });
        }
        response
            .json::<ExecuteResponse>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }

    /// Submit an async job (`POST /jobs`); returns once the node has queued it.
    pub async fn submit_job(
        &self,
        node: &RemoteNodeConfig,
        shared_token: &str,
        request: &SubmitJobRequest,
    ) -> Result<SubmitJobResponse, RemoteNodeError> {
        let url = format!("{}/jobs", node.base_url);
        let response = self
            .http
            .post(url)
            .timeout(SUBMIT_JOB_TIMEOUT)
            .bearer_auth(shared_token)
            .json(request)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteNodeError::Rejected {
                status: status.as_u16(),
                body,
            });
        }
        response
            .json::<SubmitJobResponse>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }

    /// Fetch one job's status (`GET /jobs/:id`), including its log tail.
    pub async fn get_job(
        &self,
        node: &RemoteNodeConfig,
        shared_token: &str,
        job_id: Uuid,
    ) -> Result<NodeJobStatus, RemoteNodeError> {
        let url = format!("{}/jobs/{}", node.base_url, job_id);
        let response = self
            .http
            .get(url)
            .timeout(JOB_STATUS_TIMEOUT)
            .bearer_auth(shared_token)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteNodeError::Request(format!("{status}: {body}")));
        }
        response
            .json::<NodeJobStatus>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }

    /// Request cancellation of a job (`POST /jobs/:id/cancel`).
    pub async fn cancel_job(
        &self,
        node: &RemoteNodeConfig,
        shared_token: &str,
        job_id: Uuid,
    ) -> Result<CancelJobResponse, RemoteNodeError> {
        let url = format!("{}/jobs/{}/cancel", node.base_url, job_id);
        let response = self
            .http
            .post(url)
            .timeout(JOB_STATUS_TIMEOUT)
            .bearer_auth(shared_token)
            .send()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RemoteNodeError::Request(format!("{status}: {body}")));
        }
        response
            .json::<CancelJobResponse>()
            .await
            .map_err(|e| RemoteNodeError::Request(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_node::protocol::NODE_PROTOCOL_VERSION;
    use axum::{routing::get, Json, Router};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn heartbeat_client_reads_node_status() {
        async fn heartbeat() -> Json<NodeHeartbeat> {
            Json(NodeHeartbeat {
                node_id: "babylon".to_string(),
                online: true,
                capacity_total: 2,
                capacity_available: 1,
                active_leases: 1,
                version: "test".to_string(),
                protocol_version: NODE_PROTOCOL_VERSION,
                labels: vec!["lean".to_string()],
                cpu_total: 8,
                mem_total_bytes: 0,
                mem_available_bytes: 0,
                disk_total_bytes: 0,
                disk_available_bytes: 0,
                active_jobs: 0,
                queued_jobs: 0,
                cached_toolchains: vec![],
            })
        }
        let app = Router::new().route("/heartbeat", get(heartbeat));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = RemoteNodeClient::default();
        let node = RemoteNodeConfig {
            id: "babylon".to_string(),
            base_url: format!("http://{addr}"),
            token_env: "TOKEN".to_string(),
            labels: None,
        };
        let heartbeat = client.heartbeat(&node, "unused").await.unwrap();
        assert_eq!(heartbeat.capacity_available, 1);
        assert_eq!(heartbeat.labels, vec!["lean".to_string()]);
    }
}
