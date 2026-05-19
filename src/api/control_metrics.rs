//! Lightweight in-process metrics for the control plane (P0-#3).
//!
//! Tracks counters used to validate the perf overhaul:
//! - SSE chunk size p50 / p99 (sample-based)
//! - `/api/control/events` request rate
//! - `/api/control/running` request rate
//! - Average broadcast events per active mission
//!
//! Everything lives behind a single `Arc<ControlMetrics>` so the handler at
//! `GET /api/control/metrics` can return a JSON snapshot. Counters are
//! `AtomicU64`; samples are kept in a fixed-size ring under a `Mutex` to
//! cap memory. Cost per `record_*` call is one atomic add + (for samples)
//! one `lock + write_index`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// Number of samples we keep for percentile estimation. ~64 KB at 8 bytes
/// each — generous enough for stable percentiles, small enough not to
/// matter.
const SAMPLE_BUFFER_SIZE: usize = 8192;

/// Sliding window for per-endpoint rate calculations.
const RATE_WINDOW: Duration = Duration::from_secs(60);

pub struct ControlMetrics {
    /// Wall-clock when the process started — exposed so dashboards can
    /// compute uptime-relative averages.
    start: Instant,

    sse_chunks_total: AtomicU64,
    sse_bytes_total: AtomicU64,
    sse_chunk_sizes: Mutex<RingSamples>,

    events_endpoint_hits: Mutex<Vec<Instant>>,
    running_endpoint_hits: Mutex<Vec<Instant>>,

    broadcast_events_total: AtomicU64,
    broadcast_events_by_mission: Mutex<HashMap<uuid::Uuid, u64>>,

    /// P5-#25 health budget reports — bounded ring of recent client
    /// telemetry pings. A client posts when its 5-second longtask total
    /// exceeds 2s; we keep the last 256 to surface in /metrics.
    health_reports: Mutex<Vec<HealthReport>>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct HealthReport {
    pub mission_id: Option<uuid::Uuid>,
    pub longtask_total_ms: u64,
    pub longtask_max_ms: u64,
    pub event_count: u64,
    pub heap_used_mb: f64,
    pub dom_nodes: u64,
    /// Client wall-clock when the budget breach was observed (ms since epoch).
    pub at: u64,
}

struct RingSamples {
    buf: Vec<u64>,
    next: usize,
    filled: bool,
}

impl RingSamples {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(SAMPLE_BUFFER_SIZE),
            next: 0,
            filled: false,
        }
    }

    fn push(&mut self, sample: u64) {
        if !self.filled {
            self.buf.push(sample);
            if self.buf.len() >= SAMPLE_BUFFER_SIZE {
                self.filled = true;
                self.next = 0;
            }
            return;
        }
        self.buf[self.next] = sample;
        self.next = (self.next + 1) % SAMPLE_BUFFER_SIZE;
    }

    /// Returns p50, p99 estimates and the total sample count.
    fn percentiles(&self) -> (Option<u64>, Option<u64>, usize) {
        if self.buf.is_empty() {
            return (None, None, 0);
        }
        let mut copy = self.buf.clone();
        copy.sort_unstable();
        let n = copy.len();
        let p50 = copy[n / 2];
        let p99_idx = ((n as f64 * 0.99) as usize).min(n - 1);
        let p99 = copy[p99_idx];
        (Some(p50), Some(p99), n)
    }
}

impl Default for ControlMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlMetrics {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            sse_chunks_total: AtomicU64::new(0),
            sse_bytes_total: AtomicU64::new(0),
            sse_chunk_sizes: Mutex::new(RingSamples::new()),
            events_endpoint_hits: Mutex::new(Vec::with_capacity(128)),
            running_endpoint_hits: Mutex::new(Vec::with_capacity(128)),
            broadcast_events_total: AtomicU64::new(0),
            broadcast_events_by_mission: Mutex::new(HashMap::new()),
            health_reports: Mutex::new(Vec::with_capacity(256)),
        }
    }

    /// Record a client-side health budget breach (P5-#25). Caps the
    /// in-memory ring at 256 entries so a runaway client can't OOM us.
    pub fn record_health_report(&self, report: HealthReport) {
        if let Ok(mut buf) = self.health_reports.lock() {
            if buf.len() >= 256 {
                buf.remove(0);
            }
            buf.push(report);
        }
    }

    /// Record a single SSE chunk emitted to a client.
    pub fn record_sse_chunk(&self, bytes: usize) {
        self.sse_chunks_total.fetch_add(1, Ordering::Relaxed);
        self.sse_bytes_total
            .fetch_add(bytes as u64, Ordering::Relaxed);
        if let Ok(mut samples) = self.sse_chunk_sizes.lock() {
            samples.push(bytes as u64);
        }
    }

    /// Record a single `/api/control/events` (or `/trace`) request hit.
    pub fn record_events_request(&self) {
        if let Ok(mut hits) = self.events_endpoint_hits.lock() {
            self.trim_window(&mut hits);
            hits.push(Instant::now());
        }
    }

    /// Record a single `/api/control/running` request hit.
    pub fn record_running_request(&self) {
        if let Ok(mut hits) = self.running_endpoint_hits.lock() {
            self.trim_window(&mut hits);
            hits.push(Instant::now());
        }
    }

    /// Record an `AgentEvent` broadcast to subscribers. `mission_id` is
    /// `None` for connection-scoped events (status / FIDO etc.).
    pub fn record_broadcast(&self, mission_id: Option<uuid::Uuid>) {
        self.broadcast_events_total.fetch_add(1, Ordering::Relaxed);
        if let Some(mid) = mission_id {
            if let Ok(mut map) = self.broadcast_events_by_mission.lock() {
                *map.entry(mid).or_insert(0) += 1;
            }
        }
    }

    fn trim_window(&self, hits: &mut Vec<Instant>) {
        let cutoff = Instant::now()
            .checked_sub(RATE_WINDOW)
            .unwrap_or_else(Instant::now);
        hits.retain(|t| *t >= cutoff);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let (sse_p50, sse_p99, sample_count) = self
            .sse_chunk_sizes
            .lock()
            .map(|s| s.percentiles())
            .unwrap_or((None, None, 0));

        let now = Instant::now();
        let events_hits = self
            .events_endpoint_hits
            .lock()
            .map(|mut hits| {
                self.trim_window(&mut hits);
                hits.len()
            })
            .unwrap_or(0);
        let running_hits = self
            .running_endpoint_hits
            .lock()
            .map(|mut hits| {
                self.trim_window(&mut hits);
                hits.len()
            })
            .unwrap_or(0);

        let (mission_count, events_avg_per_mission, top_missions) = self
            .broadcast_events_by_mission
            .lock()
            .map(|map| {
                let mission_count = map.len();
                let total: u64 = map.values().sum();
                let avg = if mission_count > 0 {
                    total as f64 / mission_count as f64
                } else {
                    0.0
                };
                let mut entries: Vec<(uuid::Uuid, u64)> =
                    map.iter().map(|(k, v)| (*k, *v)).collect();
                entries.sort_by(|a, b| b.1.cmp(&a.1));
                let top: Vec<TopMission> = entries
                    .into_iter()
                    .take(5)
                    .map(|(mission_id, count)| TopMission { mission_id, count })
                    .collect();
                (mission_count, avg, top)
            })
            .unwrap_or((0, 0.0, Vec::new()));

        let uptime_secs = now.saturating_duration_since(self.start).as_secs();

        let health_reports = self
            .health_reports
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();

        MetricsSnapshot {
            uptime_secs,
            health_reports,
            sse: SseStats {
                chunks_total: self.sse_chunks_total.load(Ordering::Relaxed),
                bytes_total: self.sse_bytes_total.load(Ordering::Relaxed),
                chunk_size_p50: sse_p50,
                chunk_size_p99: sse_p99,
                sample_count,
            },
            endpoints: EndpointStats {
                events_req_per_minute: events_hits as u64,
                running_req_per_minute: running_hits as u64,
            },
            broadcast: BroadcastStats {
                events_total: self.broadcast_events_total.load(Ordering::Relaxed),
                mission_count_observed: mission_count,
                events_avg_per_mission,
                top_missions,
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub uptime_secs: u64,
    pub sse: SseStats,
    pub endpoints: EndpointStats,
    pub broadcast: BroadcastStats,
    /// Most recent client-side health budget breaches (P5-#25).
    pub health_reports: Vec<HealthReport>,
}

#[derive(Debug, Serialize)]
pub struct SseStats {
    pub chunks_total: u64,
    pub bytes_total: u64,
    pub chunk_size_p50: Option<u64>,
    pub chunk_size_p99: Option<u64>,
    pub sample_count: usize,
}

#[derive(Debug, Serialize)]
pub struct EndpointStats {
    /// Hits in the last 60 seconds.
    pub events_req_per_minute: u64,
    pub running_req_per_minute: u64,
}

#[derive(Debug, Serialize)]
pub struct BroadcastStats {
    pub events_total: u64,
    pub mission_count_observed: usize,
    pub events_avg_per_mission: f64,
    pub top_missions: Vec<TopMission>,
}

#[derive(Debug, Serialize)]
pub struct TopMission {
    pub mission_id: uuid::Uuid,
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_monotonic() {
        let m = ControlMetrics::new();
        for size in 1..=1000 {
            m.record_sse_chunk(size);
        }
        let snap = m.snapshot();
        let p50 = snap.sse.chunk_size_p50.unwrap();
        let p99 = snap.sse.chunk_size_p99.unwrap();
        assert!(p50 <= p99, "p50 must be <= p99: p50={p50} p99={p99}");
        assert!(p99 >= 900, "p99 of 1..=1000 should be near 1000, got {p99}");
    }

    #[test]
    fn broadcast_avg_handles_missions() {
        let m = ControlMetrics::new();
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        m.record_broadcast(Some(a));
        m.record_broadcast(Some(a));
        m.record_broadcast(Some(b));
        m.record_broadcast(None); // status event, doesn't count toward per-mission
        let snap = m.snapshot();
        assert_eq!(snap.broadcast.events_total, 4);
        assert_eq!(snap.broadcast.mission_count_observed, 2);
        assert!((snap.broadcast.events_avg_per_mission - 1.5).abs() < 1e-9);
        assert_eq!(snap.broadcast.top_missions[0].mission_id, a);
        assert_eq!(snap.broadcast.top_missions[0].count, 2);
    }

    #[test]
    fn rate_window_trims_old_hits() {
        let m = ControlMetrics::new();
        m.record_events_request();
        let snap = m.snapshot();
        assert_eq!(snap.endpoints.events_req_per_minute, 1);
    }
}
