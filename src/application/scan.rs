use std::{
    fmt,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};

use crate::{
    application::ports::{ContextWorker, CredentialResolver, DocumentLoader},
    domain::{DryRunResult, Finding, Limits, ScanResult, Usage, WorkerRequest},
};

const MAX_ANSWER_UTF16: usize = 16_000;
const MAX_SUMMARY_UTF16: usize = 2_000;
const MAX_UNCERTAINTY_UTF16: usize = 2_000;
const MAX_FINDINGS: usize = 100;
const MAX_UNCERTAINTIES: usize = 50;

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
    )?;
    Ok((result, loaded.total_bytes, fallback))
}

pub fn analyze_documents(
    credentials: &dyn CredentialResolver,
    worker: &dyn ContextWorker,
    question: &str,
    documents: &[crate::domain::Document],
    limits: &Limits,
    primary_model: &str,
    fallback_models: &[String],
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
        attempt_limits.timeout_ms = remaining.as_millis().clamp(1, u64::MAX as u128) as u64;
        let attempt = worker.analyze(
            &WorkerRequest {
                model: (*model).to_owned(),
                question: question.trim().to_owned(),
                documents: documents.to_vec(),
                limits: attempt_limits,
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResult {
    answer: String,
    findings: Vec<Finding>,
    uncertainties: Vec<String>,
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
    for (index, finding) in parsed.findings.iter().enumerate() {
        check_utf16(
            &finding.summary,
            &format!("findings[{index}].summary"),
            MAX_SUMMARY_UTF16,
        )?;
        let Some(document) = documents.iter().find(|doc| doc.path == finding.path) else {
            bail!(
                "finding {index} references an unknown path: {}",
                finding.path
            );
        };
        let range = crate::domain::LineRange {
            start_line: finding.start_line,
            end_line: finding.end_line,
        };
        if range.start_line == 0
            || range.end_line < range.start_line
            || range.end_line > document.line_count
            || !document
                .allowed_ranges
                .iter()
                .any(|allowed| allowed.contains(range))
        {
            bail!(
                "finding {index} has an invalid line range for {}",
                finding.path
            );
        }
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
        findings: parsed.findings,
        uncertainties: parsed.uncertainties,
        files_read: documents.iter().map(|doc| doc.path.clone()).collect(),
        trust: "untrusted-model-output-with-validated-source-references".to_owned(),
        model,
        usage,
    })
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
    fn rejects_extra_model_fields() {
        let raw = json!({"answer": "x", "findings": [], "uncertainties": [], "surprise": true});
        assert!(
            validate_worker_result(
                &raw.to_string(),
                &[document(vec![LineRange {
                    start_line: 1,
                    end_line: 3
                }])],
                "m".to_owned(),
                None
            )
            .is_err()
        );
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
        )
        .unwrap();
        assert!(fallback);
        assert_eq!(result.model, "fallback");
        assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(24));
        assert_eq!(result.usage.as_ref().unwrap().cost, Some(0.002));
        assert_eq!(*worker.models.lock().unwrap(), ["primary", "fallback"]);
    }
}
