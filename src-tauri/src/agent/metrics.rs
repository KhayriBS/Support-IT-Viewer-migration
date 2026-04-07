//! System metrics collection module.
//!
//! Migrated from C# sources:
//! - `AgentMetrics.cs`      → `AgentMetrics` struct
//! - `CpuUsageReader.cs`    → `MetricsCollector::get_cpu_usage()`
//! - `MemoryUsageReader.cs` → `MetricsCollector::get_ram_usage()`
//! - `DiskUsageReader.cs`   → `MetricsCollector::get_disk_usage()`
//!
//! Key improvements over C#:
//! - No `Thread.Sleep(500)` blocking call (sysinfo refreshes async-friendly)
//! - Cross-platform (works on Linux/macOS too, not just Windows PerformanceCounter)
//! - Thread-safe via `Mutex<System>` singleton

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use sysinfo::{Disks, System};

// ─── AgentMetrics struct ──────────────────────────────────────────────────────
// Equivalent C#:
//   public class AgentMetrics {
//       public double CpuUsage { get; set; }
//       public double RamUsage { get; set; }
//       public double DiskUsage { get; set; }
//       public long Timestamp { get; set; }
//   }

/// System metrics snapshot sent to the signaling server via `POST /agents/metrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMetrics {
    /// CPU usage percentage (0.0 – 100.0)
    pub cpu_usage: f64,
    /// RAM usage percentage (0.0 – 100.0)
    pub ram_usage: f64,
    /// Disk usage percentage for the primary drive (0.0 – 100.0)
    pub disk_usage: f64,
    /// Unix timestamp in milliseconds (UTC)
    pub timestamp: i64,
}

// ─── MetricsCollector ─────────────────────────────────────────────────────────
// Replaces the three static C# classes (CpuUsageReader, MemoryUsageReader, DiskUsageReader)
// with a single collector that holds a `sysinfo::System` instance.

/// Collects system metrics (CPU, RAM, disk).
///
/// Holds a `sysinfo::System` that gets refreshed on each snapshot.
/// The C# version used Windows `PerformanceCounter` with a blocking `Thread.Sleep(500)`.
/// This version uses `sysinfo` which collects data without blocking.
pub struct MetricsCollector {
    system: Mutex<System>,
}

impl MetricsCollector {
    /// Creates a new `MetricsCollector`.
    ///
    /// Equivalent to the C# static constructors in `CpuUsageReader` and `MemoryUsageReader`
    /// which called `cpuCounter.NextValue()` to prime the counter.
    pub fn new() -> Self {
        let mut sys = System::new_all();
        // Prime the CPU measurement (like C#: cpuCounter.NextValue() in static ctor)
        sys.refresh_cpu_usage();
        Self {
            system: Mutex::new(sys),
        }
    }

    /// Collects a full metrics snapshot.
    ///
    /// Equivalent to the C# `Program.cs` block:
    /// ```csharp
    /// var metrics = new AgentMetrics {
    ///     CpuUsage  = CpuUsageReader.GetCpuUsage(),
    ///     RamUsage  = MemoryUsageReader.GetRamUsage(),
    ///     DiskUsage = DiskUsageReader.GetDiskUsage("C"),
    ///     Timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds()
    /// };
    /// ```
    pub fn collect(&self) -> AgentMetrics {
        let cpu = self.get_cpu_usage();
        let ram = self.get_ram_usage();
        let disk = self.get_disk_usage();
        let timestamp = chrono::Utc::now().timestamp_millis();

        AgentMetrics {
            cpu_usage: cpu,
            ram_usage: ram,
            disk_usage: disk,
            timestamp,
        }
    }

    /// Returns overall CPU usage as a percentage (0.0 – 100.0).
    ///
    /// C# equivalent (`CpuUsageReader.cs`):
    /// ```csharp
    /// private static readonly PerformanceCounter cpuCounter =
    ///     new PerformanceCounter("Processor", "% Processor Time", "_Total");
    /// public static float GetCpuUsage() {
    ///     Thread.Sleep(500);  // ← REMOVED: sysinfo doesn't need this
    ///     return (float)Math.Round(cpuCounter.NextValue(), 2);
    /// }
    /// ```
    pub fn get_cpu_usage(&self) -> f64 {
        let mut sys = self.system.lock().unwrap();
        sys.refresh_cpu_usage();

        // Average across all logical CPUs (like C# "_Total" instance)
        let total: f32 = sys.cpus().iter().map(|cpu| cpu.cpu_usage()).sum();
        let count = sys.cpus().len() as f32;

        if count == 0.0 {
            return 0.0;
        }

        round2((total / count) as f64)
    }

    /// Returns RAM usage as a percentage (0.0 – 100.0).
    ///
    /// C# equivalent (`MemoryUsageReader.cs`):
    /// ```csharp
    /// private static readonly PerformanceCounter ramCounter =
    ///     new PerformanceCounter("Memory", "% Committed Bytes In Use");
    /// public static float GetRamUsage() {
    ///     return (float)Math.Round(ramCounter.NextValue(), 2);
    /// }
    /// ```
    pub fn get_ram_usage(&self) -> f64 {
        let mut sys = self.system.lock().unwrap();
        sys.refresh_memory();

        let total = sys.total_memory() as f64;
        let used = sys.used_memory() as f64;

        if total == 0.0 {
            return 0.0;
        }

        round2((used / total) * 100.0)
    }

    /// Returns disk usage for the primary drive as a percentage (0.0 – 100.0).
    ///
    /// C# equivalent (`DiskUsageReader.cs`):
    /// ```csharp
    /// public static float GetDiskUsage(string driveLetter = "C") {
    ///     DriveInfo drive = new DriveInfo(driveLetter);
    ///     if (!drive.IsReady) return 0;
    ///     double total = drive.TotalSize;
    ///     double free  = drive.TotalFreeSpace;
    ///     double usedPercent = ((total - free) / total) * 100;
    ///     return (float)Math.Round(usedPercent, 2);
    /// }
    /// ```
    ///
    /// On Windows, looks for the `C:\` drive. On other platforms, looks for `/`.
    pub fn get_disk_usage(&self) -> f64 {
        let disks = Disks::new_with_refreshed_list();

        // Find the target mount point: "C:\" on Windows, "/" on Unix
        let target_mount = if cfg!(windows) { "C:\\" } else { "/" };

        for disk in disks.list() {
            let mount = disk.mount_point().to_string_lossy();

            // On Windows: mount_point() returns "C:\\"
            // On Linux/macOS: mount_point() returns "/"
            if mount.eq_ignore_ascii_case(target_mount) {
                let total = disk.total_space() as f64;
                let available = disk.available_space() as f64;

                if total == 0.0 {
                    return 0.0;
                }

                let used_percent = ((total - available) / total) * 100.0;
                return round2(used_percent);
            }
        }

        // Fallback: if target drive not found, use the first available disk
        if let Some(disk) = disks.list().first() {
            let total = disk.total_space() as f64;
            let available = disk.available_space() as f64;

            if total > 0.0 {
                return round2(((total - available) / total) * 100.0);
            }
        }

        0.0
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Rounds a float to 2 decimal places (like C#: `Math.Round(value, 2)`).
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collector_new() {
        let collector = MetricsCollector::new();
        // Should not panic
        let metrics = collector.collect();
        println!(
            "CPU: {}%, RAM: {}%, Disk: {}%, Timestamp: {}",
            metrics.cpu_usage, metrics.ram_usage, metrics.disk_usage, metrics.timestamp
        );
    }

    #[test]
    fn test_cpu_usage_in_range() {
        let collector = MetricsCollector::new();
        // Wait a bit for CPU measurement to stabilize (like C# did with Sleep)
        std::thread::sleep(std::time::Duration::from_millis(500));
        let cpu = collector.get_cpu_usage();
        assert!(cpu >= 0.0, "CPU usage should be >= 0, got {cpu}");
        assert!(cpu <= 100.0, "CPU usage should be <= 100, got {cpu}");
    }

    #[test]
    fn test_ram_usage_in_range() {
        let collector = MetricsCollector::new();
        let ram = collector.get_ram_usage();
        assert!(ram > 0.0, "RAM usage should be > 0 (some memory is always used), got {ram}");
        assert!(ram <= 100.0, "RAM usage should be <= 100, got {ram}");
    }

    #[test]
    fn test_disk_usage_in_range() {
        let collector = MetricsCollector::new();
        let disk = collector.get_disk_usage();
        assert!(disk >= 0.0, "Disk usage should be >= 0, got {disk}");
        assert!(disk <= 100.0, "Disk usage should be <= 100, got {disk}");
    }

    #[test]
    fn test_agent_metrics_serialization() {
        let metrics = AgentMetrics {
            cpu_usage: 45.32,
            ram_usage: 67.89,
            disk_usage: 82.15,
            timestamp: 1711929600000,
        };

        let json = serde_json::to_string(&metrics).unwrap();

        // Verify camelCase serialization (matches Spring Boot API expectations)
        assert!(json.contains("\"cpuUsage\""));
        assert!(json.contains("\"ramUsage\""));
        assert!(json.contains("\"diskUsage\""));
        assert!(json.contains("\"timestamp\""));

        // Verify round-trip deserialization
        let deserialized: AgentMetrics = serde_json::from_str(&json).unwrap();
        assert!((deserialized.cpu_usage - 45.32).abs() < f64::EPSILON);
        assert!((deserialized.ram_usage - 67.89).abs() < f64::EPSILON);
    }

    #[test]
    fn test_round2() {
        assert!((round2(45.3267) - 45.33).abs() < f64::EPSILON);
        assert!((round2(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((round2(100.0) - 100.0).abs() < f64::EPSILON);
        assert!((round2(99.999) - 100.0).abs() < f64::EPSILON);
    }
}
