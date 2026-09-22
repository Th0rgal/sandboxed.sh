use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use sysinfo::{Disks, Pid, ProcessesToUpdate, System};

#[derive(Serialize)]
pub struct Consumer {
    label: &'static str,
    memory: u64,
    processes: usize,
}
#[derive(Serialize)]
pub struct Snapshot {
    cpu_percent: Option<f32>,
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

#[tauri::command]
pub async fn local_machine_metrics(
    voice: tauri::State<'_, crate::voice::VoiceState>,
) -> Result<Snapshot, String> {
    let voice_pid = voice.worker_pid();
    let agents = crate::local_agents::active_pids();
    tauri::async_runtime::spawn_blocking(move || {
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
            memory_used: system.used_memory(),
            memory_total: system.total_memory(),
            disk_used: disk
                .map(|d| d.total_space().saturating_sub(d.available_space()))
                .unwrap_or(0),
            disk_total: disk.map(|d| d.total_space()).unwrap_or(0),
            consumers,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
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
