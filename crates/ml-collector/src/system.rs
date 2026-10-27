use std::collections::{HashMap, HashSet};

use ml_core::{ProcSample, SystemSample};
use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

/// Сколько программ с высокой нагрузкой по процессору и по памяти сохраняем за один снимок.
const TOP_PROCS: usize = 5;

/// Собирает нагрузку, память, сеть, диск и батарею. Температуру пока не читаем:
/// без драйверов Windows отдаёт её не на всех ПК, а опрос замедляет систему.
pub struct SystemCollector {
    sys: System,
    networks: Networks,
    disks: Disks,
    last_net: Option<(u64, u64)>,
}

impl SystemCollector {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Self {
            sys,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            last_net: None,
        }
    }

    pub fn snapshot(&mut self) -> (SystemSample, Vec<ProcSample>) {
        let ts = chrono::Utc::now().timestamp();
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.networks.refresh();
        self.disks.refresh();

        let total = self.sys.total_memory().max(1);
        let used = self.sys.used_memory();

        let (rx_now, tx_now) = self
            .networks
            .iter()
            .fold((0u64, 0u64), |(rx, tx), (_, n)| (rx + n.total_received(), tx + n.total_transmitted()));
        let (net_rx, net_tx) = match self.last_net {
            Some((rx, tx)) => (rx_now.saturating_sub(rx), tx_now.saturating_sub(tx)),
            None => (0, 0),
        };
        self.last_net = Some((rx_now, tx_now));

        let disk_free_gb = self
            .disks
            .iter()
            .find(|d| d.mount_point().to_string_lossy().to_uppercase().starts_with("C:"))
            .or_else(|| self.disks.iter().next())
            .map(|d| d.available_space() as f32 / (1024.0 * 1024.0 * 1024.0));

        let (battery_pct, on_battery) = crate::power_status();

        let sample = SystemSample {
            ts,
            cpu_pct: self.sys.global_cpu_usage(),
            mem_pct: used as f32 / total as f32 * 100.0,
            mem_used_mb: (used / (1024 * 1024)) as u32,
            net_rx,
            net_tx,
            disk_free_gb,
            battery_pct,
            on_battery,
            temp_c: None,
            uptime_s: System::uptime(),
        };
        (sample, self.top_procs(ts))
    }

    fn top_procs(&self, ts: i64) -> Vec<ProcSample> {
        let cores = self.sys.cpus().len().max(1) as f32;
        let mut agg: HashMap<String, (f32, u64)> = HashMap::new();
        for p in self.sys.processes().values() {
            let name = p.name().to_string_lossy().trim_end_matches(".exe").to_string();
            if name == "System Idle Process" {
                continue;
            }
            let e = agg.entry(name).or_insert((0.0, 0));
            e.0 += p.cpu_usage() / cores;
            e.1 += p.memory();
        }
        let mut all: Vec<(String, f32, u64)> = agg.into_iter().map(|(n, (c, m))| (n, c, m)).collect();

        let mut keep: HashSet<String> = HashSet::new();
        all.sort_by(|a, b| b.1.total_cmp(&a.1));
        keep.extend(all.iter().take(TOP_PROCS).map(|p| p.0.clone()));
        all.sort_by(|a, b| b.2.cmp(&a.2));
        keep.extend(all.iter().take(TOP_PROCS).map(|p| p.0.clone()));

        all.into_iter()
            .filter(|p| keep.contains(&p.0))
            .map(|(name, cpu, mem)| ProcSample { ts, name, cpu_pct: cpu, mem_mb: (mem / (1024 * 1024)) as u32 })
            .collect()
    }
}
