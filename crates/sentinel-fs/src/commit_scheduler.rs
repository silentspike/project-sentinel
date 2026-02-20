//! Adaptive Commit Scheduler: smooths write spikes under I/O pressure.
//!
//! Tracks commit rate and delays writes when IOPS budget is exceeded.
//! Optionally reads Linux PSI (Pressure Stall Information) from
//! `/proc/pressure/io` for system-wide I/O backpressure awareness.
//!
//! Config via `storage.toml`:
//! ```toml
//! [artifact.scheduler]
//! max_iops = 500
//! commit_delay_ms = 5
//! batch_window_ms = 10
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Default max sustained IOPS before the scheduler starts throttling.
pub const DEFAULT_MAX_IOPS: u32 = 500;

/// Default delay injected per commit when over IOPS budget.
pub const DEFAULT_COMMIT_DELAY_MS: u64 = 5;

/// Default sliding window for rate measurement.
pub const DEFAULT_BATCH_WINDOW_MS: u64 = 1000;

/// PSI I/O threshold (avg10 percentage) above which we add extra delay.
const PSI_IO_PRESSURE_THRESHOLD: f64 = 10.0;

/// Extra delay multiplier when PSI detects system-wide I/O pressure.
const PSI_PRESSURE_MULTIPLIER: u32 = 3;

/// Configuration for the adaptive commit scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum sustained commits per second before throttling.
    pub max_iops: u32,
    /// Base delay per commit when rate exceeds max_iops.
    pub commit_delay: Duration,
    /// Sliding window for rate measurement.
    pub batch_window: Duration,
    /// Whether to read `/proc/pressure/io` for PSI-aware throttling.
    pub psi_aware: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_iops: DEFAULT_MAX_IOPS,
            commit_delay: Duration::from_millis(DEFAULT_COMMIT_DELAY_MS),
            batch_window: Duration::from_millis(DEFAULT_BATCH_WINDOW_MS),
            psi_aware: cfg!(target_os = "linux"),
        }
    }
}

/// Statistics snapshot from the commit scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerStats {
    /// Total commits tracked in current window.
    pub commits_in_window: usize,
    /// Total delays injected (cumulative).
    pub delays_injected: u64,
    /// Total delay time injected (cumulative).
    pub total_delay: Duration,
    /// Current measured commit rate (per second).
    pub current_rate: f64,
    /// Whether the scheduler is currently throttling.
    pub throttling: bool,
    /// Last PSI avg10 reading (0.0 if PSI not available).
    pub psi_io_avg10: f64,
}

/// Adaptive commit scheduler that smooths write spikes.
pub struct CommitScheduler {
    config: SchedulerConfig,
    /// Timestamps of recent commits within the sliding window.
    recent_commits: VecDeque<Instant>,
    /// Cumulative stats.
    delays_injected: u64,
    total_delay: Duration,
    /// Cached PSI reading (refreshed periodically).
    psi_io_avg10: f64,
    psi_last_check: Option<Instant>,
}

impl CommitScheduler {
    /// Create a new scheduler with the given config.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            recent_commits: VecDeque::new(),
            delays_injected: 0,
            total_delay: Duration::ZERO,
            psi_io_avg10: 0.0,
            psi_last_check: None,
        }
    }

    /// Create a scheduler that never throttles (pass-through).
    pub fn noop() -> Self {
        Self::new(SchedulerConfig {
            max_iops: u32::MAX,
            commit_delay: Duration::ZERO,
            batch_window: Duration::from_secs(1),
            psi_aware: false,
        })
    }

    /// Called before each write transaction commit. May sleep to stay under IOPS budget.
    pub fn pre_commit(&mut self) {
        let now = Instant::now();
        self.prune_window(now);

        let rate = self.current_rate_inner(now);
        let max = self.config.max_iops as f64;

        if rate >= max {
            let mut delay = self.config.commit_delay;

            // PSI-aware: multiply delay if system I/O pressure is high
            if self.config.psi_aware {
                self.refresh_psi(now);
                if self.psi_io_avg10 > PSI_IO_PRESSURE_THRESHOLD {
                    delay *= PSI_PRESSURE_MULTIPLIER;
                }
            }

            std::thread::sleep(delay);
            self.delays_injected += 1;
            self.total_delay += delay;
        }

        self.recent_commits.push_back(now);
    }

    /// Get scheduler statistics.
    pub fn stats(&self) -> SchedulerStats {
        let now = Instant::now();
        SchedulerStats {
            commits_in_window: self.recent_commits.len(),
            delays_injected: self.delays_injected,
            total_delay: self.total_delay,
            current_rate: self.current_rate_inner(now),
            throttling: self.current_rate_inner(now) >= self.config.max_iops as f64,
            psi_io_avg10: self.psi_io_avg10,
        }
    }

    /// Current commit rate (commits per second) based on the sliding window.
    fn current_rate_inner(&self, now: Instant) -> f64 {
        let window_secs = self.config.batch_window.as_secs_f64();
        if window_secs <= 0.0 {
            return 0.0;
        }
        // Count commits within the window
        let cutoff = now - self.config.batch_window;
        let count = self.recent_commits.iter().filter(|&&t| t >= cutoff).count();
        count as f64 / window_secs
    }

    /// Remove commits older than the batch window.
    fn prune_window(&mut self, now: Instant) {
        let cutoff = now - self.config.batch_window;
        while let Some(&front) = self.recent_commits.front() {
            if front < cutoff {
                self.recent_commits.pop_front();
            } else {
                break;
            }
        }
    }

    /// Refresh PSI I/O reading (cached, max once per 500ms).
    fn refresh_psi(&mut self, now: Instant) {
        let should_refresh = match self.psi_last_check {
            Some(last) => now.duration_since(last) > Duration::from_millis(500),
            None => true,
        };
        if should_refresh {
            self.psi_io_avg10 = read_psi_io_avg10().unwrap_or(0.0);
            self.psi_last_check = Some(now);
        }
    }
}

/// Read the `avg10` value from `/proc/pressure/io` (Linux only).
/// Returns the 10-second average percentage of time tasks were stalled on I/O.
fn read_psi_io_avg10() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/pressure/io").ok()?;
        // Format: "some avg10=X.XX avg60=X.XX avg300=X.XX total=NNN"
        for line in content.lines() {
            if line.starts_with("some") {
                for part in line.split_whitespace() {
                    if let Some(val) = part.strip_prefix("avg10=") {
                        return val.parse::<f64>().ok();
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_scheduler_never_throttles() {
        let mut sched = CommitScheduler::noop();
        // Fire 10000 commits instantly — should never delay
        for _ in 0..10000 {
            sched.pre_commit();
        }
        let stats = sched.stats();
        assert_eq!(stats.delays_injected, 0);
        assert_eq!(stats.total_delay, Duration::ZERO);
    }

    #[test]
    fn scheduler_tracks_rate() {
        let config = SchedulerConfig {
            max_iops: 1000, // high enough to not throttle
            commit_delay: Duration::from_millis(1),
            batch_window: Duration::from_secs(1),
            psi_aware: false,
        };
        let mut sched = CommitScheduler::new(config);
        for _ in 0..50 {
            sched.pre_commit();
        }
        let stats = sched.stats();
        assert_eq!(stats.commits_in_window, 50);
        assert!(stats.current_rate > 0.0);
    }

    #[test]
    fn scheduler_injects_delay_when_over_budget() {
        let config = SchedulerConfig {
            max_iops: 10, // very low — will throttle after 10 commits/sec
            commit_delay: Duration::from_millis(1),
            batch_window: Duration::from_secs(1),
            psi_aware: false,
        };
        let mut sched = CommitScheduler::new(config);

        // First 10 commits are free (within budget)
        for _ in 0..10 {
            sched.pre_commit();
        }

        // Next commits should trigger delays
        let before = Instant::now();
        for _ in 0..5 {
            sched.pre_commit();
        }
        let elapsed = before.elapsed();

        let stats = sched.stats();
        assert!(stats.delays_injected > 0, "should have injected delays");
        assert!(
            elapsed >= Duration::from_millis(3),
            "should have slept at least 3ms"
        );
    }

    #[test]
    fn window_pruning_works() {
        let config = SchedulerConfig {
            max_iops: 100,
            commit_delay: Duration::from_millis(1),
            batch_window: Duration::from_millis(50), // very short window
            psi_aware: false,
        };
        let mut sched = CommitScheduler::new(config);

        for _ in 0..20 {
            sched.pre_commit();
        }
        assert!(sched.stats().commits_in_window > 0);

        // Wait longer than the window
        std::thread::sleep(Duration::from_millis(60));

        sched.pre_commit(); // this prunes old entries
        assert_eq!(sched.stats().commits_in_window, 1);
    }

    #[test]
    fn psi_reader_doesnt_crash() {
        // Should return Some(f64) on Linux, None elsewhere — never panic
        let result = read_psi_io_avg10();
        if cfg!(target_os = "linux") {
            // PSI might not be enabled on all kernels
            if let Some(val) = result {
                assert!(val >= 0.0);
            }
        }
    }
}
