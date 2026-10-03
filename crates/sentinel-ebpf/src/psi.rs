//! PSI (Pressure Stall Information) reader for Bio-Engine stress input.
//!
//! Reads PSI data from cgroup v2 pressure files and converts to stress factors
//! that feed into the Bio-Engine's stress model.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};
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

    /// Reads complete, valid CPU pressure for measured telemetry, without fallback values.
    pub fn read_measured_cpu_pressure(&self) -> Result<PsiMetrics> {
        self.read_measured_pressure("cpu.pressure")
    }

    /// Reads complete, valid memory pressure for measured telemetry.
    pub fn read_measured_memory_pressure(&self) -> Result<PsiMetrics> {
        self.read_measured_pressure("memory.pressure")
    }

    /// Reads complete, valid I/O pressure for measured telemetry.
    pub fn read_measured_io_pressure(&self) -> Result<PsiMetrics> {
        self.read_measured_pressure("io.pressure")
    }

    fn read_measured_pressure(&self, file: &str) -> Result<PsiMetrics> {
        let content = std::fs::read_to_string(format!("{}/{file}", self.cgroup_path))?;
        parse_measured_psi(&content)
    }
}

fn parse_measured_psi(content: &str) -> Result<PsiMetrics> {
    let mut lines = content
        .lines()
        .filter(|line| line.split_whitespace().next() == Some("some"));
    let line = lines.next().context("Missing measured PSI some line")?;
    if lines.next().is_some() {
        bail!("Duplicate measured PSI some line");
    }
    let mut averages = [None; 3];
    let mut total = None;
    let mut fields = HashSet::new();
    for part in line.split_whitespace().skip(1) {
        let (key, value) = part
            .split_once('=')
            .context("Malformed measured PSI field")?;
        if key.is_empty() || value.is_empty() || !fields.insert(key) {
            bail!("Invalid or duplicate measured PSI field: {key}");
        }
        let index = match key {
            "avg10" => Some(0),
            "avg60" => Some(1),
            "avg300" => Some(2),
            _ => None,
        };
        if let Some(index) = index {
            let value = value
                .parse::<f64>()
                .context("Malformed measured PSI average")?;
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                bail!("Measured PSI average outside finite 0..100 range");
            }
            averages[index] = Some(value);
        } else if key == "total" {
            if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
                bail!("Malformed measured PSI total");
            }
            total = Some(
                value
                    .parse::<u64>()
                    .context("Malformed measured PSI total")?,
            );
        }
    }
    Ok(PsiMetrics {
        avg10: averages[0].context("Missing measured PSI avg10")?,
        avg60: averages[1].context("Missing measured PSI avg60")?,
        avg300: averages[2].context("Missing measured PSI avg300")?,
        total: total.context("Missing measured PSI total")?,
    })
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
pub fn combined_stress_factor(cpu: &PsiMetrics, memory: &PsiMetrics, io: &PsiMetrics) -> f32 {
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
        assert_relative_eq!(
            combined_stress_factor(&cpu, &memory, &io),
            0.56,
            epsilon = 0.01
        );
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
    fn measured_psi_requires_complete_unique_finite_observations() {
        for content in [
            "",
            "some",
            "some avg60=0 avg300=0 total=0",
            "some avg10=0 avg300=0 total=0",
            "some avg10=0 avg60=0 total=0",
            "some avg10=0 avg60=0 avg300=0",
            "some avg10=0 avg60=0 avg300=0 total=0 avg10=1",
            "some avg10=0 avg60=0 avg300=0 total=0 total=1",
            "some avg10=NaN avg60=0 avg300=0 total=0",
            "some avg10=0 avg60=inf avg300=0 total=0",
            "some avg10=0 avg60=0 avg300=-inf total=0",
            "some avg10=-1 avg60=0 avg300=0 total=0",
            "some avg10=0 avg60=101 avg300=0 total=0",
            "some avg10=bad avg60=0 avg300=0 total=0",
            "some avg10=0 avg60=0 avg300=0 total=-1",
            "some avg10=0 avg60=0 avg300=0 total=1.5",
            "some avg10=0 avg60=0 avg300=0 total=0\nsome avg10=1 avg60=1 avg300=1 total=1",
        ] {
            assert!(parse_measured_psi(content).is_err(), "{content}");
        }
        let zero = parse_measured_psi("some avg10=0 avg60=0 avg300=0 total=0").unwrap();
        assert_eq!([zero.avg10, zero.avg60, zero.avg300], [0.0; 3]);
        assert_eq!(zero.total, 0);
        let metrics =
            parse_measured_psi("some avg300=100 avg60=0.25 avg10=80 total=10\nfull avg10=0")
                .unwrap();
        assert_eq!(
            [metrics.avg10, metrics.avg60, metrics.avg300],
            [80.0, 0.25, 100.0]
        );
        // The shared parser remains permissive for existing non-telemetry callers.
        assert_eq!(parse_psi("some").unwrap().avg10, 0.0);
    }

    #[test]
    #[ignore] // Requires real cgroup filesystem
    fn read_real_cpu_pressure() {
        let reader = PsiReader::new("/proc/pressure");
        let _metrics = reader.read_cpu_pressure().unwrap();
    }
}
