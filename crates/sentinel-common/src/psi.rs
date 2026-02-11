//! PSI (Pressure Stall Information) types and parsing.
//!
//! Shared between sentinel-sandbox (cgroups) and sentinel-ebpf (monitoring).

use anyhow::{anyhow, Result};

/// PSI (Pressure Stall Information) Metriken.
#[derive(Debug, Clone, Default)]
pub struct PsiMetrics {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total: u64,
}

/// Parsed eine PSI-Zeile im Format: "some avg10=0.00 avg60=0.00 avg300=0.00 total=0"
pub fn parse_psi(content: &str) -> Result<PsiMetrics> {
    // Finde die erste Zeile die mit "some" beginnt
    let line = content
        .lines()
        .find(|l| l.starts_with("some"))
        .ok_or_else(|| anyhow!("No 'some' line found in PSI content"))?;

    let mut metrics = PsiMetrics::default();

    // Parse die Werte
    for part in line.split_whitespace().skip(1) {
        // skip "some"
        if let Some((key, value)) = part.split_once('=') {
            match key {
                "avg10" => metrics.avg10 = value.parse()?,
                "avg60" => metrics.avg60 = value.parse()?,
                "avg300" => metrics.avg300 = value.parse()?,
                "total" => metrics.total = value.parse()?,
                _ => {}
            }
        }
    }

    Ok(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_parse_some_line() {
        let content =
            "some avg10=1.50 avg60=2.30 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0";
        let metrics = parse_psi(content).unwrap();
        assert_eq!(metrics.avg10, 1.50);
        assert_eq!(metrics.avg60, 2.30);
        assert_eq!(metrics.avg300, 0.10);
        assert_eq!(metrics.total, 12345);
    }

    #[test]
    fn psi_parse_missing_some_line() {
        let content = "full avg10=0.00 avg60=0.00 avg300=0.00 total=0";
        assert!(parse_psi(content).is_err());
    }

    #[test]
    fn psi_default_is_zero() {
        let metrics = PsiMetrics::default();
        assert_eq!(metrics.avg10, 0.0);
        assert_eq!(metrics.avg60, 0.0);
        assert_eq!(metrics.avg300, 0.0);
        assert_eq!(metrics.total, 0);
    }
}
