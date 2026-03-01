//! Resource monitoring via cgroup v2 files.
//!
//! Captures memory, CPU, and I/O stats during benchmark execution by reading
//! cgroup v2 files at `/sys/fs/cgroup/`. When cgroup files are unavailable
//! (e.g. local macOS development), all reads return `None` and stats default
//! to zero.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

const CGROUP_PATH: &str = "/sys/fs/cgroup";
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Resource usage statistics captured during a benchmark run.
#[derive(Debug, Clone, Default)]
pub struct ResourceStats {
    pub wall_time: Duration,
    pub peak_memory_bytes: u64,
    pub start_memory_bytes: u64,
    pub end_memory_bytes: u64,
    pub avg_memory_bytes: u64,
    pub cpu_user_usec: u64,
    pub cpu_sys_usec: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub sample_count: u32,
}

impl fmt::Display for ResourceStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "| Metric | Value |")?;
        writeln!(f, "|--------|-------|")?;
        writeln!(f, "| Wall time | {:.1}s |", self.wall_time.as_secs_f64())?;
        writeln!(
            f,
            "| Peak memory | {} |",
            format_bytes(self.peak_memory_bytes)
        )?;
        writeln!(
            f,
            "| Avg memory | {} |",
            format_bytes(self.avg_memory_bytes)
        )?;
        writeln!(
            f,
            "| CPU user | {:.1}s |",
            self.cpu_user_usec as f64 / 1_000_000.0
        )?;
        writeln!(
            f,
            "| CPU sys | {:.1}s |",
            self.cpu_sys_usec as f64 / 1_000_000.0
        )?;
        writeln!(f, "| Disk read | {} |", format_bytes(self.io_read_bytes))?;
        write!(f, "| Disk write | {} |", format_bytes(self.io_write_bytes))
    }
}

/// Format a byte count as a human-readable string.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Format a resource stats section for inclusion in a PR comment.
pub fn format_resource_comment(label: &str, stats: &ResourceStats) -> String {
    format!("**{label}**\n{stats}\n")
}

#[derive(Debug)]
struct CpuStat {
    user_usec: u64,
    system_usec: u64,
}

#[derive(Debug)]
struct IoStat {
    read_bytes: u64,
    write_bytes: u64,
}

/// Monitors cgroup v2 resource usage during benchmark execution.
pub struct CgroupMonitor {
    start_time: Instant,
    start_memory: u64,
    start_cpu: Option<CpuStat>,
    start_io: Option<IoStat>,
    stop_flag: Arc<AtomicBool>,
    peak_memory: Arc<AtomicU64>,
    memory_sum: Arc<AtomicU64>,
    sample_count: Arc<AtomicU64>,
    poll_handle: JoinHandle<()>,
}

impl CgroupMonitor {
    /// Begin monitoring. Snapshots current cgroup values as baselines and
    /// spawns a background polling task for memory tracking.
    pub fn start() -> Self {
        let start_memory = read_memory_current().unwrap_or(0);
        let start_cpu = read_cpu_stat();
        let start_io = read_io_stat();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let peak_memory = Arc::new(AtomicU64::new(start_memory));
        let memory_sum = Arc::new(AtomicU64::new(start_memory));
        let sample_count = Arc::new(AtomicU64::new(1));

        let poll_handle = {
            let stop = stop_flag.clone();
            let peak = peak_memory.clone();
            let sum = memory_sum.clone();
            let count = sample_count.clone();

            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    tokio::time::sleep(POLL_INTERVAL).await;
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(current) = read_memory_current() {
                        peak.fetch_max(current, Ordering::Relaxed);
                        sum.fetch_add(current, Ordering::Relaxed);
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };

        CgroupMonitor {
            start_time: Instant::now(),
            start_memory,
            start_cpu,
            start_io,
            stop_flag,
            peak_memory,
            memory_sum,
            sample_count,
            poll_handle,
        }
    }

    /// Stop monitoring and compute delta statistics.
    pub async fn finish(self) -> ResourceStats {
        let wall_time = self.start_time.elapsed();

        self.stop_flag.store(true, Ordering::Relaxed);
        let _ = self.poll_handle.await;

        let end_memory = read_memory_current().unwrap_or(0);
        let end_cpu = read_cpu_stat();
        let end_io = read_io_stat();

        let peak = self.peak_memory.load(Ordering::Relaxed).max(end_memory);
        let total_sum = self.memory_sum.load(Ordering::Relaxed) + end_memory;
        let total_count = self.sample_count.load(Ordering::Relaxed) + 1;
        let avg = if total_count > 0 {
            total_sum / total_count
        } else {
            0
        };

        let (cpu_user, cpu_sys) = match (self.start_cpu, end_cpu) {
            (Some(start), Some(end)) => (
                end.user_usec.saturating_sub(start.user_usec),
                end.system_usec.saturating_sub(start.system_usec),
            ),
            _ => (0, 0),
        };

        let (io_read, io_write) = match (self.start_io, end_io) {
            (Some(start), Some(end)) => (
                end.read_bytes.saturating_sub(start.read_bytes),
                end.write_bytes.saturating_sub(start.write_bytes),
            ),
            _ => (0, 0),
        };

        ResourceStats {
            wall_time,
            peak_memory_bytes: peak,
            start_memory_bytes: self.start_memory,
            end_memory_bytes: end_memory,
            avg_memory_bytes: avg,
            cpu_user_usec: cpu_user,
            cpu_sys_usec: cpu_sys,
            io_read_bytes: io_read,
            io_write_bytes: io_write,
            sample_count: total_count as u32,
        }
    }
}

// --- cgroup v2 file readers ---

fn read_memory_current() -> Option<u64> {
    std::fs::read_to_string(format!("{CGROUP_PATH}/memory.current"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn read_cpu_stat() -> Option<CpuStat> {
    let content = std::fs::read_to_string(format!("{CGROUP_PATH}/cpu.stat")).ok()?;
    let mut user_usec = 0u64;
    let mut system_usec = 0u64;

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        match parts.next()? {
            "user_usec" => user_usec = parts.next()?.parse().ok()?,
            "system_usec" => system_usec = parts.next()?.parse().ok()?,
            _ => {}
        }
    }

    Some(CpuStat {
        user_usec,
        system_usec,
    })
}

fn read_io_stat() -> Option<IoStat> {
    let content = std::fs::read_to_string(format!("{CGROUP_PATH}/io.stat")).ok()?;
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;

    for line in content.lines() {
        for field in line.split_whitespace() {
            if let Some(val) = field.strip_prefix("rbytes=") {
                read_bytes += val.parse::<u64>().unwrap_or(0);
            } else if let Some(val) = field.strip_prefix("wbytes=") {
                write_bytes += val.parse::<u64>().unwrap_or(0);
            }
        }
    }

    Some(IoStat {
        read_bytes,
        write_bytes,
    })
}

#[cfg(test)]
fn parse_memory_current(content: &str) -> Option<u64> {
    content.trim().parse().ok()
}

#[cfg(test)]
fn parse_cpu_stat(content: &str) -> Option<CpuStat> {
    let mut user_usec = 0u64;
    let mut system_usec = 0u64;

    for line in content.lines() {
        let mut parts = line.split_whitespace();
        if let Some(key) = parts.next() {
            if let Some(val) = parts.next() {
                match key {
                    "user_usec" => user_usec = val.parse().ok()?,
                    "system_usec" => system_usec = val.parse().ok()?,
                    _ => {}
                }
            }
        }
    }

    Some(CpuStat {
        user_usec,
        system_usec,
    })
}

#[cfg(test)]
fn parse_io_stat(content: &str) -> Option<IoStat> {
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;

    for line in content.lines() {
        for field in line.split_whitespace() {
            if let Some(val) = field.strip_prefix("rbytes=") {
                read_bytes += val.parse::<u64>().unwrap_or(0);
            } else if let Some(val) = field.strip_prefix("wbytes=") {
                write_bytes += val.parse::<u64>().unwrap_or(0);
            }
        }
    }

    Some(IoStat {
        read_bytes,
        write_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes_values() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(1048576), "1.0 MiB");
        assert_eq!(format_bytes(1073741824), "1.0 GiB");
        assert_eq!(format_bytes(1288490189), "1.2 GiB");
    }

    #[test]
    fn test_resource_stats_display() {
        let stats = ResourceStats {
            wall_time: Duration::from_secs_f64(42.3),
            peak_memory_bytes: 1288490189,
            start_memory_bytes: 100_000_000,
            end_memory_bytes: 200_000_000,
            avg_memory_bytes: 858993459,
            cpu_user_usec: 38_100_000,
            cpu_sys_usec: 2_400_000,
            io_read_bytes: 536_870_912,
            io_write_bytes: 12_897_484,
            sample_count: 42,
        };
        let output = stats.to_string();
        assert!(output.contains("| Wall time | 42.3s |"));
        assert!(output.contains("| Peak memory | 1.2 GiB |"));
        assert!(output.contains("| Avg memory |"));
        assert!(output.contains("| CPU user | 38.1s |"));
        assert!(output.contains("| CPU sys | 2.4s |"));
        assert!(output.contains("| Disk read | 512.0 MiB |"));
        assert!(output.contains("| Disk write | 12.3 MiB |"));
    }

    #[test]
    fn test_parse_memory_current() {
        assert_eq!(parse_memory_current("123456789\n"), Some(123456789));
        assert_eq!(parse_memory_current("0\n"), Some(0));
        assert_eq!(parse_memory_current("not_a_number\n"), None);
    }

    #[test]
    fn test_parse_cpu_stat() {
        let content = "\
usage_usec 100000
user_usec 80000
system_usec 20000
nr_periods 0
nr_throttled 0
throttled_usec 0
";
        let stat = parse_cpu_stat(content).unwrap();
        assert_eq!(stat.user_usec, 80000);
        assert_eq!(stat.system_usec, 20000);
    }

    #[test]
    fn test_parse_io_stat() {
        let content = "259:0 rbytes=1048576 wbytes=524288 rios=100 wios=50 dbytes=0 dios=0\n\
                        259:1 rbytes=2097152 wbytes=1048576 rios=200 wios=100 dbytes=0 dios=0\n";
        let stat = parse_io_stat(content).unwrap();
        assert_eq!(stat.read_bytes, 1048576 + 2097152);
        assert_eq!(stat.write_bytes, 524288 + 1048576);
    }

    #[test]
    fn test_parse_io_stat_empty() {
        let stat = parse_io_stat("").unwrap();
        assert_eq!(stat.read_bytes, 0);
        assert_eq!(stat.write_bytes, 0);
    }

    #[test]
    fn test_format_resource_comment() {
        let stats = ResourceStats {
            wall_time: Duration::from_secs(10),
            ..Default::default()
        };
        let comment = format_resource_comment("base (merge-base)", &stats);
        assert!(comment.contains("**base (merge-base)**"));
        assert!(comment.contains("| Wall time | 10.0s |"));
    }

    #[tokio::test]
    async fn test_monitor_returns_stats() {
        let monitor = CgroupMonitor::start();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let stats = monitor.finish().await;
        assert!(stats.wall_time >= Duration::from_millis(50));
        assert!(stats.sample_count >= 2);
    }
}
