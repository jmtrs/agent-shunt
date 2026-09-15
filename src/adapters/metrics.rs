use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{application::ports::MetricsSink, domain::MetricRecord};

pub struct JsonlMetrics {
    path: PathBuf,
}

impl JsonlMetrics {
    pub fn default_path() -> PathBuf {
        dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .unwrap_or_else(std::env::temp_dir)
            .join("agent-shunt/metrics.jsonl")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn summary(&self) -> Result<MetricsSummary> {
        if !self.path.exists() {
            return Ok(MetricsSummary::default());
        }
        let file = std::fs::File::open(&self.path)?;
        let mut summary = MetricsSummary::default();
        for line in BufReader::new(file).lines() {
            let Ok(record) = serde_json::from_str::<MetricRecord>(&line?) else {
                continue;
            };
            summary.runs += 1;
            summary.successes += u64::from(record.success);
            summary.failures += u64::from(!record.success);
            summary.fallback_runs += u64::from(record.fallback);
            summary.total_duration_ms += record.duration_ms;
            summary.total_tokens += record.total_tokens.unwrap_or(0);
            summary.total_cost += record.cost.unwrap_or(0.0);
        }
        Ok(summary)
    }
}

impl MetricsSink for JsonlMetrics {
    fn record(&self, metric: &MetricRecord) -> Result<()> {
        let parent = self.path.parent().context("metrics path has no parent")?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        serde_json::to_writer(&mut file, metric)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSummary {
    pub runs: u64,
    pub successes: u64,
    pub failures: u64,
    pub fallback_runs: u64,
    pub total_duration_ms: u128,
    pub total_tokens: u64,
    pub total_cost: f64,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tempfile::tempdir;

    use crate::{application::ports::MetricsSink, domain::MetricRecord};

    use super::JsonlMetrics;

    #[test]
    fn persists_only_aggregate_metric_fields() {
        let root = tempdir().unwrap();
        let path = root.path().join("metrics.jsonl");
        let metrics = JsonlMetrics::new(path.clone());
        metrics
            .record(&MetricRecord {
                timestamp: Utc::now(),
                operation: "scan".to_owned(),
                success: true,
                model: Some("worker".to_owned()),
                duration_ms: 10,
                input_bytes: 100,
                files: 2,
                prompt_tokens: Some(20),
                completion_tokens: Some(5),
                total_tokens: Some(25),
                cost: Some(0.001),
                fallback: false,
                error_kind: None,
            })
            .unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(!content.contains("question"));
        assert!(!content.contains("path"));
        assert_eq!(metrics.summary().unwrap().total_tokens, 25);
    }
}
