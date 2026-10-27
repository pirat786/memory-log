//! Расчёты по снимкам ПК: нагрузка, сеть, ритм дня, состояние.

use std::collections::HashMap;

use chrono::{Local, TimeZone, Timelike};
use serde::Serialize;

use crate::model::{ProcSample, Session, SystemSample, Timestamp};
use crate::summary::merge_blocks;

/// Перерыв — пауза между сессиями дольше этого времени.
pub const BREAK_MIN_SECS: i64 = 5 * 60;
const SAMPLE_SECS: i64 = 10;

#[derive(Debug, Clone, Serialize)]
pub struct Range {
    pub min: f32,
    pub avg: f32,
    pub max: f32,
    pub min_ts: Timestamp,
    pub max_ts: Timestamp,
}

fn range(values: impl Iterator<Item = (Timestamp, f32)>) -> Option<Range> {
    let mut n = 0u32;
    let mut sum = 0f64;
    let mut min: Option<(Timestamp, f32)> = None;
    let mut max: Option<(Timestamp, f32)> = None;
    for (ts, v) in values {
        n += 1;
        sum += v as f64;
        if min.map_or(true, |m| v < m.1) {
            min = Some((ts, v));
        }
        if max.map_or(true, |m| v > m.1) {
            max = Some((ts, v));
        }
    }
    let (min_ts, min) = min?;
    let (max_ts, max) = max?;
    Some(Range { min, avg: (sum / n as f64) as f32, max, min_ts, max_ts })
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadStats {
    pub cpu: Range,
    pub mem: Range,
}

pub fn load_stats(samples: &[SystemSample]) -> Option<LoadStats> {
    Some(LoadStats {
        cpu: range(samples.iter().map(|s| (s.ts, s.cpu_pct)))?,
        mem: range(samples.iter().map(|s| (s.ts, s.mem_pct)))?,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct HourNet {
    pub hour: u32,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetStats {
    pub rx_total: u64,
    pub tx_total: u64,
    pub by_hour: Vec<HourNet>,
    pub peak_hour: Option<u32>,
}

fn hour_of(ts: Timestamp) -> usize {
    Local.timestamp_opt(ts, 0).single().map(|d| d.hour() as usize).unwrap_or(0)
}

pub fn net_stats(samples: &[SystemSample]) -> Option<NetStats> {
    if samples.is_empty() {
        return None;
    }
    let mut by_hour: Vec<HourNet> = (0..24).map(|hour| HourNet { hour, rx: 0, tx: 0 }).collect();
    for s in samples {
        let h = &mut by_hour[hour_of(s.ts)];
        h.rx += s.net_rx;
        h.tx += s.net_tx;
    }
    let rx_total = by_hour.iter().map(|h| h.rx).sum();
    let tx_total = by_hour.iter().map(|h| h.tx).sum();
    let peak = by_hour.iter().max_by_key(|h| h.rx + h.tx).filter(|h| h.rx + h.tx > 0).map(|h| h.hour);
    Some(NetStats { rx_total, tx_total, by_hour, peak_hour: peak })
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcLoad {
    pub name: String,
    pub avg_cpu: f32,
    pub max_cpu: f32,
    pub max_cpu_ts: Timestamp,
    pub max_mem_mb: u32,
    pub max_mem_ts: Timestamp,
}

/// Программы с самой высокой нагрузкой. Среднее считается по снимкам, где программа попала в топ.
pub fn proc_loads(procs: &[ProcSample], limit: usize) -> Vec<ProcLoad> {
    struct Acc {
        n: u32,
        cpu_sum: f64,
        max_cpu: f32,
        max_cpu_ts: Timestamp,
        max_mem: u32,
        max_mem_ts: Timestamp,
    }
    let mut map: HashMap<&str, Acc> = HashMap::new();
    for p in procs {
        let a = map.entry(p.name.as_str()).or_insert(Acc { n: 0, cpu_sum: 0.0, max_cpu: 0.0, max_cpu_ts: p.ts, max_mem: 0, max_mem_ts: p.ts });
        a.n += 1;
        a.cpu_sum += p.cpu_pct as f64;
        if p.cpu_pct > a.max_cpu {
            a.max_cpu = p.cpu_pct;
            a.max_cpu_ts = p.ts;
        }
        if p.mem_mb > a.max_mem {
            a.max_mem = p.mem_mb;
            a.max_mem_ts = p.ts;
        }
    }
    let mut out: Vec<ProcLoad> = map
        .into_iter()
        .map(|(name, a)| ProcLoad {
            name: name.to_string(),
            avg_cpu: (a.cpu_sum / a.n as f64) as f32,
            max_cpu: a.max_cpu,
            max_cpu_ts: a.max_cpu_ts,
            max_mem_mb: a.max_mem,
            max_mem_ts: a.max_mem_ts,
        })
        .collect();
    out.sort_by(|a, b| b.max_cpu.total_cmp(&a.max_cpu));
    out.truncate(limit);
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Break {
    pub start: Timestamp,
    pub end: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rhythm {
    pub first_ts: Timestamp,
    pub last_ts: Timestamp,
    pub breaks: Vec<Break>,
    pub break_secs: i64,
    pub switches: u32,
    pub longest_focus_secs: i64,
}

pub fn rhythm(sessions: &[Session]) -> Option<Rhythm> {
    let first = sessions.first()?;
    let last = sessions.last()?;
    let mut breaks = Vec::new();
    let mut switches = 0u32;
    for pair in sessions.windows(2) {
        let gap = pair[1].start - pair[0].end;
        if gap >= BREAK_MIN_SECS {
            breaks.push(Break { start: pair[0].end, end: pair[1].start });
        } else if pair[0].app != pair[1].app {
            switches += 1;
        }
    }
    let break_secs = breaks.iter().map(|b| b.end - b.start).sum();
    let longest_focus_secs = merge_blocks(sessions).iter().map(|b| b.end - b.start).max().unwrap_or(0);
    Some(Rhythm { first_ts: first.start, last_ts: last.end, breaks, break_secs, switches, longest_focus_secs })
}

#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub uptime_secs: u64,
    pub disk_free_start: Option<f32>,
    pub disk_free_end: Option<f32>,
    pub battery_min: Option<u8>,
    pub on_battery_secs: i64,
    pub temp_max: Option<f32>,
}

pub fn health(samples: &[SystemSample]) -> Option<Health> {
    let first = samples.first()?;
    let last = samples.last()?;
    let mut on_battery_secs = 0i64;
    for (i, s) in samples.iter().enumerate() {
        if s.on_battery == Some(true) {
            on_battery_secs += samples.get(i + 1).map_or(SAMPLE_SECS, |n| (n.ts - s.ts).clamp(0, 3 * SAMPLE_SECS));
        }
    }
    Some(Health {
        uptime_secs: samples.iter().map(|s| s.uptime_s).max().unwrap_or(0),
        disk_free_start: first.disk_free_gb,
        disk_free_end: last.disk_free_gb,
        battery_min: samples.iter().filter_map(|s| s.battery_pct).min(),
        on_battery_secs,
        temp_max: samples.iter().filter_map(|s| s.temp_c).fold(None, |m: Option<f32>, t| Some(m.map_or(t, |x| x.max(t)))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Category;

    fn sys(ts: i64, cpu: f32, rx: u64) -> SystemSample {
        SystemSample {
            ts,
            cpu_pct: cpu,
            mem_pct: 50.0,
            mem_used_mb: 8000,
            net_rx: rx,
            net_tx: 1,
            disk_free_gb: Some(100.0),
            battery_pct: None,
            on_battery: None,
            temp_c: None,
            uptime_s: ts as u64,
        }
    }

    fn session(start: i64, end: i64, app: &str) -> Session {
        Session { start, end, app: app.into(), title: String::new(), category: Category::Work }
    }

    #[test]
    fn load_min_avg_max() {
        let l = load_stats(&[sys(0, 10.0, 0), sys(10, 90.0, 0), sys(20, 20.0, 0)]).unwrap();
        assert_eq!(l.cpu.min, 10.0);
        assert_eq!(l.cpu.max, 90.0);
        assert_eq!(l.cpu.max_ts, 10);
        assert!((l.cpu.avg - 40.0).abs() < 0.01);
    }

    #[test]
    fn net_totals() {
        let n = net_stats(&[sys(0, 1.0, 100), sys(10, 1.0, 50)]).unwrap();
        assert_eq!(n.rx_total, 150);
        assert_eq!(n.tx_total, 2);
        assert!(n.peak_hour.is_some());
    }

    #[test]
    fn rhythm_breaks_and_switches() {
        let s = vec![session(0, 100, "a"), session(100, 200, "b"), session(1000, 1100, "b")];
        let r = rhythm(&s).unwrap();
        assert_eq!(r.switches, 1);
        assert_eq!(r.breaks.len(), 1);
        assert_eq!(r.break_secs, 800);
    }

    #[test]
    fn proc_peaks() {
        let p = vec![
            ProcSample { ts: 1, name: "x".into(), cpu_pct: 10.0, mem_mb: 100 },
            ProcSample { ts: 2, name: "x".into(), cpu_pct: 30.0, mem_mb: 50 },
        ];
        let l = proc_loads(&p, 5);
        assert_eq!(l[0].max_cpu_ts, 2);
        assert_eq!(l[0].max_mem_mb, 100);
        assert!((l[0].avg_cpu - 20.0).abs() < 0.01);
    }
}
