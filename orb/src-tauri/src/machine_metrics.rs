use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use sysinfo::{Disks, Pid, ProcessesToUpdate, System};

#[derive(Clone, Serialize)]
pub struct Consumer {
    label: &'static str,
    memory: u64,
    processes: usize,
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    cpu_percent: Option<f32>,
    gpu_percent: Option<f32>,
    memory_used: u64,
    memory_total: u64,
    disk_used: u64,
    disk_total: u64,
    consumers: Vec<Consumer>,
}

fn category(pid: Pid, system: &System, voice: Option<u32>, agents: &[u32]) -> Option<usize> {
    let mut current = Some(pid);
    for _ in 0..64 {
        let p = current?;
        if voice == Some(p.as_u32()) {
            return Some(1);
        }
        if agents.contains(&p.as_u32()) {
            return Some(2);
        }
        if p.as_u32() == std::process::id() {
            return Some(0);
        }
        current = system.process(p).and_then(|p| p.parent());
    }
    None
}

#[derive(Clone, Serialize)]
pub struct Sample {
    time: u64,
    cpu: Option<f32>,
    gpu: Option<f32>,
    memory: Option<f64>,
}
#[derive(Clone, Serialize)]
pub struct CachedSnapshot {
    #[serde(flatten)]
    snapshot: Snapshot,
    history: Vec<Sample>,
}
static CACHE: OnceLock<Mutex<Option<CachedSnapshot>>> = OnceLock::new();

/// The native process owns sampling, not the lifetime of a webview/page.
pub fn start(voice: crate::voice::VoiceState) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(move || loop {
            if let Ok(snapshot) = collect(voice.worker_pid(), crate::local_agents::active_pids()) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(None)).lock() {
                    let mut history = cache
                        .as_ref()
                        .map(|s| s.history.clone())
                        .unwrap_or_default();
                    history.retain(|s| s.time >= now.saturating_sub(60_000));
                    history.push(Sample {
                        time: now,
                        cpu: snapshot.cpu_percent,
                        gpu: snapshot.gpu_percent,
                        memory: (snapshot.memory_total > 0).then(|| {
                            snapshot.memory_used as f64 / snapshot.memory_total as f64 * 100.0
                        }),
                    });
                    *cache = Some(CachedSnapshot { snapshot, history });
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
    });
}
#[tauri::command]
pub fn local_machine_metrics() -> Result<CachedSnapshot, String> {
    CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Local metrics are warming up".to_string())
}

fn collect(voice_pid: Option<u32>, agents: Vec<u32>) -> Result<Snapshot, String> {
    static SYSTEM: OnceLock<Mutex<(System, bool)>> = OnceLock::new();
    let mut state = SYSTEM
        .get_or_init(|| Mutex::new((System::new(), false)))
        .lock()
        .map_err(|e| e.to_string())?;
    let (system, initialized) = &mut *state;
    system.refresh_cpu_usage();
    system.refresh_memory();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let cpu_percent = initialized.then(|| system.global_cpu_usage());
    *initialized = true;
    let disks = Disks::new_with_refreshed_list();
    // macOS root and Data share an APFS container; count only the Data mount.
    let disk = disks
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/System/Volumes/Data"))
        .or_else(|| {
            disks
                .iter()
                .find(|d| d.mount_point() == std::path::Path::new("/"))
        });
    let mut consumers = vec![
        Consumer {
            label: "Orb · native process tree",
            memory: 0,
            processes: 0,
        },
        Consumer {
            label: "Cohere · speech to text",
            memory: 0,
            processes: 0,
        },
        Consumer {
            label: "Local harnesses · started by Orb",
            memory: 0,
            processes: 0,
        },
    ];
    for (pid, process) in system.processes() {
        if let Some(index) = category(*pid, system, voice_pid, &agents) {
            consumers[index].memory += process.memory();
            consumers[index].processes += 1;
        }
    }
    Ok(Snapshot {
        cpu_percent,
        gpu_percent: apple_gpu_usage(),
        memory_used: system.used_memory(),
        memory_total: system.total_memory(),
        disk_used: disk
            .map(|d| d.total_space().saturating_sub(d.available_space()))
            .unwrap_or(0),
        disk_total: disk.map(|d| d.total_space()).unwrap_or(0),
        consumers,
    })
}

fn apple_gpu_usage() -> Option<f32> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-r", "-c", "IOAccelerator", "-l"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let value = text
            .split("\"Device Utilization %\"=")
            .nth(1)?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<f32>()
            .ok()?;
        return (0.0..=100.0).contains(&value).then_some(value);
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_collector_reads_host_resources() {
        let sample = collect(None, Vec::new()).expect("native system collector");
        assert!(sample.memory_total > 0);
        assert!(sample.memory_used <= sample.memory_total);
        if let Some(gpu) = sample.gpu_percent {
            assert!((0.0..=100.0).contains(&gpu));
        }
        println!("Native GPU utilization: {:?}", sample.gpu_percent);
    }
    #[test]
    fn specialized_consumers_are_not_counted_as_orb() {
        let system = System::new();
        let pid = Pid::from_u32(std::process::id());
        assert_eq!(category(pid, &system, None, &[]), Some(0));
        assert_eq!(category(pid, &system, Some(pid.as_u32()), &[]), Some(1));
        assert_eq!(category(pid, &system, None, &[pid.as_u32()]), Some(2));
    }
    #[test]
    fn unrelated_process_is_not_attributed() {
        assert_eq!(category(Pid::from_u32(0), &System::new(), None, &[]), None);
    }
}
