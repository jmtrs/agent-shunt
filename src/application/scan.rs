use std::{
    fmt,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};

use crate::{
    application::ports::{ContextWorker, CredentialResolver, DocumentLoader},
    domain::{DryRunResult, Finding, Limits, LineRange, ScanResult, Usage, WorkerRequest},
};

const MAX_ANSWER_UTF16: usize = 16_000;
const MAX_SUMMARY_UTF16: usize = 2_000;
const MAX_UNCERTAINTY_UTF16: usize = 2_000;
const MAX_FINDINGS: usize = 100;
const MAX_UNCERTAINTIES: usize = 50;
/// Milliseconds of the overall worker deadline reserved for each fallback model
/// still to try. A slow primary otherwise consumes the whole deadline and the
/// fallback — a different provider, the one that helps when the primary hangs —
/// never runs. Real failures cluster at slow-primary timeouts, so guaranteeing
/// the fallback a slice is what converts those into a second, likely-healthy try.
const FALLBACK_RESERVE_MS: u64 = 20_000;
/// Floor for a single attempt's timeout, so reserving for fallbacks never starves
/// the current model below a usable budget (only capped by what remains).
const MIN_ATTEMPT_MS: u64 = 5_000;
/// When retrieval hands the worker a file as several nearby chunks, a correct
/// finding often spans two of them (and the small gap between). Merging allowed
/// ranges separated by at most this many lines lets such a finding validate,
/// while ranges far apart stay distinct so the guard still rejects references to
/// unretrieved regions.
const ALLOWED_RANGE_MERGE_GAP: usize = 16;

/// Coalesces sorted allowed ranges whose gap is within [`ALLOWED_RANGE_MERGE_GAP`].
fn merge_adjacent_ranges(ranges: &[LineRange]) -> Vec<LineRange> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| (range.start_line, range.end_line));
    let mut merged: Vec<LineRange> = Vec::with_capacity(sorted.len());
    for range in sorted {
        if let Some(last) = merged.last_mut()
            && range.start_line <= last.end_line.saturating_add(ALLOWED_RANGE_MERGE_GAP) + 1
        {
            last.end_line = last.end_line.max(range.end_line);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[derive(Debug)]
pub struct FallbackExhausted {
    failures: Vec<String>,
    usage: Option<Usage>,
}

impl FallbackExhausted {
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }
}

impl fmt::Display for FallbackExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "all worker models failed; host fallback required: {}",
            self.failures.join("; ")
        )
    }
}

impl std::error::Error for FallbackExhausted {}

#[derive(Debug, Clone)]
pub struct ScanInput {
    pub question: String,
    pub paths: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub model: String,
    pub fallback_models: Vec<String>,
    pub limits: Limits,
}

pub fn dry_run(loader: &dyn DocumentLoader, input: &ScanInput) -> Result<(DryRunResult, usize)> {
    validate_question(&input.question, &input.limits)?;
    let loaded = loader.load(&input.cwd, &input.paths, &input.limits)?;
    Ok((
        DryRunResult {
            version: 1,
            dry_run: true,
            model: input.model.trim().to_owned(),
            files_read: loaded
                .documents
                .iter()
                .map(|doc| doc.path.clone())
                .collect(),
            total_bytes: loaded.total_bytes,
        },
        loaded.total_bytes,
    ))
}

pub fn execute(
    loader: &dyn DocumentLoader,
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    input: &ScanInput,
) -> Result<(ScanResult, usize, bool)> {
    validate_question(&input.question, &input.limits)?;
    let loaded = loader.load(&input.cwd, &input.paths, &input.limits)?;
    let (result, fallback) = analyze_documents(
        credentials,
        worker,
        &input.question,
        &loaded.documents,
        &input.limits,
        &input.model,
        &input.fallback_models,
        false,
    )?;
    Ok((result, loaded.total_bytes, fallback))
}

#[allow(clippy::too_many_arguments)]
pub fn analyze_documents(
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    question: &str,
    documents: &[crate::domain::Document],
    limits: &Limits,
    primary_model: &str,
    fallback_models: &[String],
    review: bool,
) -> Result<(ScanResult, bool)> {
    let credential = credentials.resolve()?;
    let models = std::iter::once(primary_model.trim())
        .chain(fallback_models.iter().map(|model| model.trim()))
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    let mut aggregate_usage: Option<Usage> = None;
    let deadline = Instant::now() + Duration::from_millis(limits.timeout_ms);
    for (index, model) in models.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            failures.push("overall worker deadline exceeded".to_owned());
            break;
        }
        let mut attempt_limits = limits.clone();
        // Reserve time for each fallback still to try, so a slow primary cannot
        // consume the whole deadline and starve them. The fallback is a
        // different provider and is exactly what should get a real attempt when
        // the primary hangs — the dominant failure mode. The current model still
        // keeps the majority of the budget, and a fast success returns before
        // its slice is spent, so this only caps pathological latency. The floor
        // keeps a nearly-exhausted attempt usable rather than sub-second.
        let remaining_ms = remaining.as_millis().clamp(1, u64::MAX as u128) as u64;
        let pending_fallbacks = (models.len() - index - 1) as u64;
        let reserve_ms = FALLBACK_RESERVE_MS.saturating_mul(pending_fallbacks);
        attempt_limits.timeout_ms = remaining_ms
            .saturating_sub(reserve_ms)
            .max(remaining_ms.min(MIN_ATTEMPT_MS));
        let attempt = worker.analyze(
            &WorkerRequest {
                model: (*model).to_owned(),
                question: question.trim().to_owned(),
                documents: documents.to_vec(),
                limits: attempt_limits,
                review,
            },
            &credential.api_key,
        );
        let attempt = match attempt {
            Ok(response) => {
                if let Some(usage) = &response.usage {
                    aggregate_usage
                        .get_or_insert_with(Usage::default)
                        .merge(usage);
                }
                validate_worker_result(
                    &response.content,
                    documents,
                    response.response_model,
                    aggregate_usage.clone(),
                )
            }
            Err(error) => Err(error),
        };
        match attempt {
            Ok(result) => return Ok((result, index > 0)),
            Err(error) => failures.push(format!("{model}: {error}")),
        }
    }
    Err(FallbackExhausted {
        failures,
        usage: aggregate_usage,
    }
    .into())
}

pub fn validate_question(question: &str, limits: &Limits) -> Result<()> {
    let question = question.trim();
    if question.is_empty() {
        bail!("question must not be empty");
    }
    if question.len() > limits.max_question_bytes {
        bail!("question exceeds {} bytes", limits.max_question_bytes);
    }
    Ok(())
}

// Lenient by design for `json_object` mode (providers that do not enforce
// strict `json_schema`): unknown keys are ignored, every field defaults when
// omitted, and a scalar where a list is expected is coerced. A model that skips
// `findings` or writes `uncertainties` as one string must not fail the whole
// response — the real guardrail is `validate_worker_result`, which checks each
// finding's path and line range against the supplied source. In strict
// `json_schema` mode none of this triggers (the schema already constrains shape).
#[derive(serde::Deserialize)]
struct RawResult {
    #[serde(default)]
    answer: String,
    #[serde(default, deserialize_with = "value_seq")]
    findings: Vec<Finding>,
    #[serde(default, deserialize_with = "string_seq")]
    uncertainties: Vec<String>,
}

/// Deserializes a `Vec<T>` tolerantly: an explicit `null` or a single object
/// becomes an empty/one-element list rather than a type error.
fn value_seq<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(serde::de::Error::custom))
            .collect(),
        other => Ok(vec![
            serde_json::from_value(other).map_err(serde::de::Error::custom)?,
        ]),
    }
}

/// Deserializes a `Vec<String>` tolerantly: a bare string becomes a one-element
/// list, `null` becomes empty, and non-string array items are stringified.
fn string_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    let stringify = |item: serde_json::Value| match item {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    };
    Ok(match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::String(text) => vec![text],
        serde_json::Value::Array(items) => items.into_iter().map(stringify).collect(),
        other => vec![stringify(other)],
    })
}

pub fn validate_worker_result(
    raw: &str,
    documents: &[crate::domain::Document],
    model: String,
    usage: Option<Usage>,
) -> Result<ScanResult> {
    let parsed: RawResult =
        serde_json::from_str(raw).map_err(|_| anyhow::anyhow!("model returned invalid JSON"))?;
    check_utf16(&parsed.answer, "answer", MAX_ANSWER_UTF16)?;
    if parsed.findings.len() > MAX_FINDINGS || parsed.uncertainties.len() > MAX_UNCERTAINTIES {
        bail!("model response contains too many items");
    }
    // Drop-and-disclose: a finding that fails source validation is removed
    // and reported in `dropped_findings` rather than failing the whole
    // response, so one hallucinated reference cannot cost the valid ones.
    // Only when *every* finding is ungrounded is the response rejected (and
    // the model-fallback loop retried) — then nothing in it was verified.
    let mut findings = Vec::with_capacity(parsed.findings.len());
    let mut dropped_findings = Vec::new();
    for mut finding in parsed.findings {
        match validate_finding(&finding, documents) {
            Ok(()) => {
                finding.verify_hint = Some(format!(
                    "sed -n '{},{}p' {}",
                    finding.start_line,
                    finding.end_line,
                    shell_quoted(&finding.path)
                ));
                findings.push(finding);
            }
            Err(reason) => dropped_findings.push(crate::domain::DroppedFinding { finding, reason }),
        }
    }
    if !dropped_findings.is_empty() && findings.is_empty() {
        let reasons = dropped_findings
            .iter()
            .map(|dropped| dropped.reason.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("all findings were dropped: {reasons}");
    }
    for (index, uncertainty) in parsed.uncertainties.iter().enumerate() {
        check_utf16(
            uncertainty,
            &format!("uncertainties[{index}]"),
            MAX_UNCERTAINTY_UTF16,
        )?;
    }
    Ok(ScanResult {
        version: 1,
        answer: parsed.answer,
        findings,
        dropped_findings,
        uncertainties: parsed.uncertainties,
        files_read: documents.iter().map(|doc| doc.path.clone()).collect(),
        trust: "untrusted-model-output-with-validated-source-references".to_owned(),
        model,
        usage,
    })
}

/// Validates one finding against the retrieved documents, returning a stable
/// reason token on failure.
fn validate_finding(
    finding: &crate::domain::Finding,
    documents: &[crate::domain::Document],
) -> std::result::Result<(), String> {
    if check_utf16(&finding.summary, "finding.summary", MAX_SUMMARY_UTF16).is_err() {
        return Err("summary too long".to_owned());
    }
    let Some(document) = documents.iter().find(|doc| doc.path == finding.path) else {
        return Err("unknown path".to_owned());
    };
    let range = crate::domain::LineRange {
        start_line: finding.start_line,
        end_line: finding.end_line,
    };
    if range.start_line == 0
        || range.end_line < range.start_line
        || range.end_line > document.line_count
        || !merge_adjacent_ranges(&document.allowed_ranges)
            .iter()
            .any(|allowed| allowed.contains(range))
    {
        return Err("line range not in retrieved context".to_owned());
    }
    Ok(())
}

/// Quotes a path for a copy-paste shell command: bare when it needs no
/// quoting, single-quoted with escaping otherwise. A leading `-` is always
/// quoted so the command cannot parse the path as a flag.
fn shell_quoted(path: &str) -> String {
    if !path.starts_with('-')
        && path.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
        })
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

fn check_utf16(value: &str, field: &str, maximum: usize) -> Result<()> {
    if value.encode_utf16().count() > maximum {
        bail!("model response field {field} exceeds {maximum} characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result;
    use serde_json::json;

    use crate::{
        application::ports::{ContextWorker, CredentialResolver, ResolvedCredential},
        domain::{Document, Limits, LineRange, Usage, WorkerRequest, WorkerResponse},
    };

    use super::{analyze_documents, validate_question, validate_worker_result};

    fn document(allowed_ranges: Vec<LineRange>) -> Document {
        Document {
            path: "src/a.rs".to_owned(),
            bytes: 5,
            line_count: 3,
            lines: vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
            numbered_content: "1: one\n2: two\n3: three".to_owned(),
            allowed_ranges,
        }
    }

    #[test]
    fn rejects_empty_and_oversized_questions() {
        let limits = crate::domain::Limits {
            max_question_bytes: 3,
            ..crate::domain::Limits::default()
        };
        assert!(validate_question("  ", &limits).is_err());
        assert!(validate_question("four", &limits).is_err());
    }

    #[test]
    fn accepts_grounded_result() {
        let raw = json!({
            "answer": "found",
            "findings": [{"path": "src/a.rs", "startLine": 1, "endLine": 2, "summary": "evidence"}],
            "uncertainties": []
        });
        let result = validate_worker_result(
            &raw.to_string(),
            &[document(vec![LineRange {
                start_line: 1,
                end_line: 3,
            }])],
            "worker".to_owned(),
            None,
        )
        .unwrap();
        assert_eq!(result.findings[0].end_line, 2);
    }

    #[test]
    fn accepts_finding_spanning_a_small_gap_but_not_a_large_one() {
        let doc = |ranges: Vec<LineRange>| Document {
            path: "src/a.rs".to_owned(),
            bytes: 10,
            line_count: 300,
            lines: vec![String::new(); 300],
            numbered_content: String::new(),
            allowed_ranges: ranges,
        };
        let spanning = json!({
            "answer": "found",
            "findings": [{"path": "src/a.rs", "startLine": 12, "endLine": 51, "summary": "x"}],
            "uncertainties": []
        });
        // Two nearby chunks (gap of 6) merge, so a finding across them validates.
        assert!(
            validate_worker_result(
                &spanning.to_string(),
                &[doc(vec![
                    LineRange {
                        start_line: 1,
                        end_line: 33
                    },
                    LineRange {
                        start_line: 40,
                        end_line: 67
                    },
                ])],
                "m".to_owned(),
                None,
            )
            .is_ok()
        );
        // Chunks far apart do not merge: a reference into the unretrieved gap is
        // still rejected as ungrounded.
        assert!(
            validate_worker_result(
                &spanning.to_string(),
                &[doc(vec![
                    LineRange {
                        start_line: 1,
                        end_line: 20
                    },
                    LineRange {
                        start_line: 200,
                        end_line: 220
                    },
                ])],
                "m".to_owned(),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_paths_and_unseen_ranges() {
        let unknown = json!({
            "answer": "found",
            "findings": [{"path": "missing.rs", "startLine": 1, "endLine": 1, "summary": "x"}],
            "uncertainties": []
        });
        let docs = [document(vec![LineRange {
            start_line: 2,
            end_line: 2,
        }])];
        assert!(validate_worker_result(&unknown.to_string(), &docs, "m".to_owned(), None).is_err());
        let unseen = json!({
            "answer": "found",
            "findings": [{"path": "src/a.rs", "startLine": 1, "endLine": 1, "summary": "x"}],
            "uncertainties": []
        });
        assert!(validate_worker_result(&unseen.to_string(), &docs, "m".to_owned(), None).is_err());
    }

    #[test]
    fn drops_ungrounded_findings_but_keeps_valid_ones() {
        // One valid finding and two invalid ones: the response is accepted,
        // the valid finding gains a verify hint, and the invalid ones are
        // disclosed with stable reasons instead of shown or fatal.
        let raw = json!({
            "answer": "partly grounded",
            "findings": [
                {"path": "src/a.rs", "startLine": 1, "endLine": 2, "summary": "evidence"},
                {"path": "missing.rs", "startLine": 1, "endLine": 1, "summary": "hallucinated"},
                {"path": "src/a.rs", "startLine": 99, "endLine": 100, "summary": "beyond file"}
            ],
            "uncertainties": []
        });
        let docs = [document(vec![LineRange {
            start_line: 1,
            end_line: 3,
        }])];
        let result = validate_worker_result(&raw.to_string(), &docs, "m".to_owned(), None).unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].verify_hint.as_deref(),
            Some("sed -n '1,2p' src/a.rs")
        );
        assert_eq!(result.dropped_findings.len(), 2);
        assert_eq!(result.dropped_findings[0].reason, "unknown path");
        assert_eq!(
            result.dropped_findings[1].reason,
            "line range not in retrieved context"
        );
    }

    #[test]
    fn tolerates_extra_model_fields() {
        // Providers in `json_object` mode (no strict schema) may echo extra
        // keys at the top level (e.g. `sources`) or inside a finding. These are
        // ignored, not fatal: the guardrail is path/line-range validation.
        let raw = json!({
            "answer": "x",
            "uncertainties": [],
            "sources": ["echoed", "input"],
            "findings": [
                {"path": "src/a.rs", "startLine": 1, "endLine": 2, "summary": "s", "confidence": 0.9}
            ]
        });
        let result = validate_worker_result(
            &raw.to_string(),
            &[document(vec![LineRange {
                start_line: 1,
                end_line: 3,
            }])],
            "m".to_owned(),
            None,
        )
        .expect("extra fields must be ignored, not rejected");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].start_line, 1);
    }

    #[test]
    fn tolerates_missing_fields_and_scalar_lists() {
        // json_object providers may omit `findings` and write `uncertainties`
        // as a bare string; both must be accepted and normalized.
        let raw = json!({"answer": "x", "uncertainties": "just one note"});
        let result = validate_worker_result(&raw.to_string(), &[], "m".to_owned(), None)
            .expect("missing findings and scalar uncertainties must be tolerated");
        assert!(result.findings.is_empty());
        assert_eq!(result.uncertainties, vec!["just one note".to_owned()]);

        // Explicit nulls collapse to empty lists rather than erroring.
        let nulls = json!({"answer": "x", "findings": null, "uncertainties": null});
        let result = validate_worker_result(&nulls.to_string(), &[], "m".to_owned(), None).unwrap();
        assert!(result.findings.is_empty() && result.uncertainties.is_empty());
    }

    struct Credential;
    impl CredentialResolver for Credential {
        fn resolve(&self) -> Result<ResolvedCredential> {
            Ok(ResolvedCredential {
                api_key: "secret".to_owned(),
                source: "test".to_owned(),
            })
        }
    }

    struct FailingThenValidWorker {
        models: Mutex<Vec<String>>,
    }
    impl ContextWorker for FailingThenValidWorker {
        fn analyze(&self, request: &WorkerRequest, _: &str) -> Result<WorkerResponse> {
            self.models.lock().unwrap().push(request.model.clone());
            let content = if request.model == "primary" {
                "not json".to_owned()
            } else {
                json!({"answer":"fallback","findings":[],"uncertainties":[]}).to_string()
            };
            Ok(WorkerResponse {
                content,
                response_model: request.model.clone(),
                usage: Some(Usage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(2),
                    total_tokens: Some(12),
                    cost: Some(0.001),
                }),
            })
        }
    }

    #[test]
    fn validation_failure_uses_next_model() {
        let worker = FailingThenValidWorker {
            models: Mutex::new(Vec::new()),
        };
        let (result, fallback) = analyze_documents(
            &Credential,
            &worker,
            "inspect",
            &[document(vec![LineRange {
                start_line: 1,
                end_line: 3,
            }])],
            &Limits::default(),
            "primary",
            &["fallback".to_owned()],
            false,
        )
        .unwrap();
        assert!(fallback);
        assert_eq!(result.model, "fallback");
        assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(24));
        assert_eq!(result.usage.as_ref().unwrap().cost, Some(0.002));
        assert_eq!(*worker.models.lock().unwrap(), ["primary", "fallback"]);
    }

    /// Records the per-attempt timeout the chain hands each model, then fails, so
    /// the deadline split can be asserted.
    struct TimeoutRecordingWorker {
        timeouts: Mutex<Vec<u64>>,
    }
    impl ContextWorker for TimeoutRecordingWorker {
        fn analyze(&self, request: &WorkerRequest, _: &str) -> Result<WorkerResponse> {
            self.timeouts
                .lock()
                .unwrap()
                .push(request.limits.timeout_ms);
            anyhow::bail!("boom")
        }
    }

    /// A slow primary must not consume the whole deadline: the primary's
    /// attempt is capped so each pending fallback keeps its reserved slice, and
    /// the last model gets whatever remains.
    #[test]
    fn deadline_is_reserved_across_the_fallback_chain() {
        let worker = TimeoutRecordingWorker {
            timeouts: Mutex::new(Vec::new()),
        };
        let limits = Limits {
            timeout_ms: 60_000,
            ..Limits::default()
        };
        let _ = analyze_documents(
            &Credential,
            &worker,
            "inspect",
            &[document(vec![LineRange {
                start_line: 1,
                end_line: 3,
            }])],
            &limits,
            "primary",
            &["fallback".to_owned()],
            false,
        );
        let timeouts = worker.timeouts.lock().unwrap();
        assert_eq!(timeouts.len(), 2);
        // Primary reserves FALLBACK_RESERVE_MS for the one pending fallback, so
        // it gets at most the deadline minus the reserve (mock fails instantly,
        // so the fallback then sees nearly the full reserve remaining).
        assert!(timeouts[0] <= 60_000 - super::FALLBACK_RESERVE_MS);
        assert!(timeouts[0] >= super::MIN_ATTEMPT_MS);
    }
}
