//! Bounded host telemetry, independent of heartbeat clients and job lifetimes.
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub time: i64,
    pub cpu: Option<f32>,
    pub memory: Option<f64>,
    pub gpu: Option<f32>,
}
static HISTORY: OnceLock<Mutex<Vec<Sample>>> = OnceLock::new();
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub fn snapshot() -> Vec<Sample> {
    HISTORY
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map(|v| {
            v.iter()
                .filter(|s| s.time >= now() - 60_000)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
pub fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            let mut system = sysinfo::System::new();
            let mut initialized = false;
            loop {
                system.refresh_cpu_usage();
                system.refresh_memory();
                let sample = Sample {
                    time: now(),
                    cpu: initialized.then(|| system.global_cpu_usage()),
                    memory: (system.total_memory() > 0).then(|| {
                        system.used_memory() as f64 / system.total_memory() as f64 * 100.0
                    }),
                    gpu: gpu_usage(),
                };
                initialized = true;
                if let Ok(mut history) = HISTORY.get_or_init(|| Mutex::new(Vec::new())).lock() {
                    history.retain(|s| s.time >= sample.time - 60_000);
                    history.push(sample);
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        });
    });
}
fn gpu_usage() -> Option<f32> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    for _ in 0..40 {
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                return None;
            }
            let mut text = String::new();
            child
                .stdout
                .take()?
                .take(4096)
                .read_to_string(&mut text)
                .ok()?;
            return text
                .lines()
                .filter_map(|v| v.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
                .reduce(f32::max);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}
