//! PSI (Pressure Stall Information) reader for Bio-Engine stress input.
//!
//! Reads PSI data from cgroup v2 pressure files and converts to stress factors
//! that feed into the Bio-Engine's stress model.

use anyhow::Result;
use sentinel_common::psi::{parse_psi, PsiMetrics};

/// Reads PSI metrics for a specific agent cgroup.
#[derive(Debug, Clone)]
pub struct PsiReader {
    /// Path to the agent's cgroup directory.
    cgroup_path: String,
}

impl PsiReader {
    /// Creates a new PSI reader for the given cgroup path.
    pub fn new(cgroup_path: &str) -> Self {
        Self {
            cgroup_path: cgroup_path.to_string(),
        }
    }

    /// Returns the cgroup path.
    pub fn cgroup_path(&self) -> &str {
        &self.cgroup_path
    }

    /// Reads cpu.pressure for this cgroup.
    ///
    /// Returns parsed PSI metrics (avg10, avg60, avg300, total).
    pub fn read_cpu_pressure(&self) -> Result<PsiMetrics> {
        let path = format!("{}/cpu.pressure", self.cgroup_path);
        let content = std::fs::read_to_string(&path)?;
        parse_psi(&content)
    }

    /// Reads memory.pressure for this cgroup.
    pub fn read_memory_pressure(&self) -> Result<PsiMetrics> {
        let path = format!("{}/memory.pressure", self.cgroup_path);
        let content = std::fs::read_to_string(&path)?;
        parse_psi(&content)
    }

    /// Reads io.pressure for this cgroup.
    pub fn read_io_pressure(&self) -> Result<PsiMetrics> {
        let path = format!("{}/io.pressure", self.cgroup_path);
        let content = std::fs::read_to_string(&path)?;
        parse_psi(&content)
    }
}

/// Converts PSI avg10 value (0-100%) to a Bio-Engine stress factor (0.0-1.0).
///
/// Mapping:
/// - 0-10%: low stress (0.0-0.1)
/// - 10-50%: moderate stress (0.1-0.5)
/// - 50-80%: high stress (0.5-0.8)
/// - 80-100%: critical stress (0.8-1.0)
pub fn psi_to_stress_factor(psi: &PsiMetrics) -> f32 {
    (psi.avg10 as f32 / 100.0).clamp(0.0, 1.0)
}

/// Computes a combined stress factor from CPU, memory, and I/O pressure.
///
/// Weights: CPU 0.5, Memory 0.3, I/O 0.2
pub fn combined_stress_factor(
    cpu: &PsiMetrics,
    memory: &PsiMetrics,
    io: &PsiMetrics,
) -> f32 {
    let cpu_stress = psi_to_stress_factor(cpu);
    let mem_stress = psi_to_stress_factor(memory);
    let io_stress = psi_to_stress_factor(io);
    (cpu_stress * 0.5 + mem_stress * 0.3 + io_stress * 0.2).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn psi_to_stress_zero_pressure() {
        let psi = PsiMetrics::default();
        assert_relative_eq!(psi_to_stress_factor(&psi), 0.0, epsilon = 0.01);
    }

    #[test]
    fn psi_to_stress_50_percent() {
        let psi = PsiMetrics {
            avg10: 50.0,
            ..Default::default()
        };
        assert_relative_eq!(psi_to_stress_factor(&psi), 0.5, epsilon = 0.01);
    }

    #[test]
    fn psi_to_stress_100_percent() {
        let psi = PsiMetrics {
            avg10: 100.0,
            ..Default::default()
        };
        assert_relative_eq!(psi_to_stress_factor(&psi), 1.0, epsilon = 0.01);
    }

    #[test]
    fn psi_to_stress_clamps_above_100() {
        let psi = PsiMetrics {
            avg10: 150.0,
            ..Default::default()
        };
        assert_relative_eq!(psi_to_stress_factor(&psi), 1.0, epsilon = 0.01);
    }

    #[test]
    fn combined_stress_weights() {
        let cpu = PsiMetrics {
            avg10: 80.0,
            ..Default::default()
        };
        let memory = PsiMetrics {
            avg10: 40.0,
            ..Default::default()
        };
        let io = PsiMetrics {
            avg10: 20.0,
            ..Default::default()
        };
        // 0.8*0.5 + 0.4*0.3 + 0.2*0.2 = 0.4 + 0.12 + 0.04 = 0.56
        assert_relative_eq!(combined_stress_factor(&cpu, &memory, &io), 0.56, epsilon = 0.01);
    }

    #[test]
    fn combined_stress_all_zero() {
        let zero = PsiMetrics::default();
        assert_relative_eq!(
            combined_stress_factor(&zero, &zero, &zero),
            0.0,
            epsilon = 0.01
        );
    }

    #[test]
    fn combined_stress_all_max() {
        let max = PsiMetrics {
            avg10: 100.0,
            ..Default::default()
        };
        // 1.0*0.5 + 1.0*0.3 + 1.0*0.2 = 1.0
        assert_relative_eq!(
            combined_stress_factor(&max, &max, &max),
            1.0,
            epsilon = 0.01
        );
    }

    #[test]
    fn psi_reader_path() {
        let reader = PsiReader::new("/sys/fs/cgroup/sentinel/agent-01");
        assert_eq!(reader.cgroup_path(), "/sys/fs/cgroup/sentinel/agent-01");
    }

    #[test]
    #[ignore] // Requires real cgroup filesystem
    fn read_real_cpu_pressure() {
        let reader = PsiReader::new("/proc/pressure");
        let _metrics = reader.read_cpu_pressure().unwrap();
    }
}
