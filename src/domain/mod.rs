use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub timeout_ms: u64,
    pub max_output_tokens: u64,
    pub max_response_bytes: usize,
    pub max_question_bytes: usize,
    pub max_request_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            max_output_tokens: 2_000,
            max_response_bytes: 1_000_000,
            max_question_bytes: 16_000,
            max_request_bytes: 4_000_000,
            max_files: 30,
            max_file_bytes: 512_000,
            max_total_bytes: 2_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineRange {
    pub start_line: usize,
    pub end_line: usize,
}

impl LineRange {
    pub fn contains(self, other: Self) -> bool {
        self.start_line <= other.start_line && self.end_line >= other.end_line
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub path: String,
    pub bytes: usize,
    pub line_count: usize,
    pub lines: Vec<String>,
    pub numbered_content: String,
    pub allowed_ranges: Vec<LineRange>,
}

impl Document {
    pub fn numbered_range(&self, range: LineRange) -> String {
        self.lines[(range.start_line - 1)..range.end_line]
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}: {line}", range.start_line + index))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct LoadedDocuments {
    pub documents: Vec<Document>,
    pub total_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub line: usize,
    pub score: usize,
    pub matched_terms: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievedChunk {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: usize,
    pub estimated_tokens: usize,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrieveResult {
    pub version: u8,
    pub query_terms: Vec<String>,
    pub chunks: Vec<RetrievedChunk>,
    pub estimated_tokens: usize,
    pub truncated: bool,
}

// No `deny_unknown_fields`: a worker in `json_object` mode may attach extra
// keys to a finding. They are ignored on read; the path and line range are
// validated separately in `validate_worker_result`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub version: u8,
    pub answer: String,
    pub findings: Vec<Finding>,
    pub uncertainties: Vec<String>,
    pub files_read: Vec<String>,
    pub trust: String,
    pub model: String,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunResult {
    pub version: u8,
    pub dry_run: bool,
    pub model: String,
    pub files_read: Vec<String>,
    pub total_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct WorkerRequest {
    pub model: String,
    pub question: String,
    pub documents: Vec<Document>,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct WorkerResponse {
    pub content: String,
    pub response_model: String,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl Usage {
    pub fn merge(&mut self, other: &Self) {
        self.prompt_tokens = sum_options(self.prompt_tokens, other.prompt_tokens);
        self.completion_tokens = sum_options(self.completion_tokens, other.completion_tokens);
        self.total_tokens = sum_options(self.total_tokens, other.total_tokens);
        self.cost = match (self.cost, other.cost) {
            (Some(left), Some(right)) => Some(left + right),
            (left, right) => left.or(right),
        };
    }
}

fn sum_options(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (left, right) => left.or(right),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub operation: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub duration_ms: u128,
    pub input_bytes: usize,
    pub files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    pub fallback: bool,
    /// Stable failure category on `success == false` (e.g. `http_5xx`,
    /// `timeout`, `fallback_exhausted`). A fixed token, never a raw error
    /// message, so no source path or content can leak into the metrics log.
    /// Absent (and `default` on read) for successful runs and legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub home: PathBuf,
    pub skill_changed: bool,
    pub hook_changed: bool,
    pub hook_backup: Option<PathBuf>,
    pub requires_hook_trust: bool,
}
