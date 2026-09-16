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

    /// Expands a hit to its enclosing indentation block: the nearest earlier
    /// non-blank line at a strictly smaller indent (the definition header),
    /// down through every line more indented than that header, then upward over
    /// the header's own signature (Allman braces / multi-line signatures) and
    /// any decorators, attributes, or doc-comments directly above it. Works on
    /// brace and indent styles alike, with no language grammar. Returns `None`
    /// when no smaller-indent header exists (a top-level statement) or the block
    /// would exceed `max_span` lines, so the caller keeps its fixed window
    /// instead of swallowing a whole impl or file.
    pub fn enclosing_block(&self, line: usize, max_span: usize) -> Option<LineRange> {
        enclosing_block(&self.lines, line, max_span)
    }

    /// Trims blank and delimiter-only lines (`{`, `}`, `;`, ...) from a range's
    /// edges — the padding a fixed window drags in — never emptying it: at
    /// least one line always survives.
    pub fn trim_trivial(&self, range: LineRange) -> LineRange {
        let is_trivial = |line: &str| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed
                    .chars()
                    .all(|character| "{}()[];,".contains(character))
        };
        let mut start = range.start_line;
        let mut end = range.end_line;
        while start < end && is_trivial(&self.lines[start - 1]) {
            start += 1;
        }
        while end > start && is_trivial(&self.lines[end - 1]) {
            end -= 1;
        }
        LineRange {
            start_line: start,
            end_line: end,
        }
    }
}

/// Language-agnostic enclosing-block resolver over raw lines. Expands a hit to
/// its enclosing indentation block: the nearest earlier non-blank line at a
/// strictly smaller indent (the definition header), down through every line more
/// indented than that header, then upward over the header's own signature
/// (Allman braces / multi-line signatures) and any decorators, attributes, or
/// doc-comments directly above it. Returns `None` when no smaller-indent header
/// exists (a top-level statement) or the block would exceed `max_span` lines, so
/// the caller keeps its fixed window instead of swallowing a whole impl or file.
/// The heuristic fallback for languages the AST resolver does not cover.
pub fn enclosing_block(lines: &[String], line: usize, max_span: usize) -> Option<LineRange> {
    let index = line.checked_sub(1)?;
    let hit_indent = line_indent(lines.get(index)?)?;
    // Header: nearest earlier non-blank line indented less than the hit.
    let mut cursor = index;
    let header = loop {
        if cursor == 0 {
            return None;
        }
        cursor -= 1;
        if let Some(current) = line_indent(&lines[cursor])
            && current < hit_indent
        {
            break (cursor, current);
        }
    };
    let (start, header_indent) = header;
    // Body: extend down while lines stay more indented than the header, spanning
    // blank lines but stopping at the first line that returns to the header's
    // level or below (the closing brace or the next sibling).
    let mut end = index;
    let mut cursor = index;
    while cursor + 1 < lines.len() {
        cursor += 1;
        match line_indent(&lines[cursor]) {
            Some(current) if current > header_indent => end = cursor,
            Some(_) => break,
            None => {}
        }
    }
    // Signature: a delimiter-led header (a lone `{`, or a `) -> T {` continuation)
    // is not the real definition line — the signature sits above it. Absorb the
    // contiguous run of non-blank lines at or beyond the header's indent, so an
    // Allman brace or a multi-line signature keeps its `fn foo(...)` line.
    let mut start = start;
    if is_continuation_line(&lines[start]) {
        while start > 0 {
            match line_indent(&lines[start - 1]) {
                Some(above) if above >= header_indent => start -= 1,
                _ => break,
            }
        }
    }
    // Decorators, attributes, and doc-comments directly above the header at its
    // own indent belong to the definition (`@app.route`, `#[test]`, `///`), so a
    // hit in the body still carries what the code *is*.
    while start > 0 {
        let above = &lines[start - 1];
        match line_indent(above) {
            Some(indent) if indent == header_indent && is_annotation_line(above) => {
                start -= 1;
            }
            _ => break,
        }
    }
    let range = LineRange {
        start_line: start + 1,
        end_line: end + 1,
    };
    (range.end_line - range.start_line < max_span).then_some(range)
}

/// Indentation width of a line in bytes, or `None` for a blank line (one made
/// only of whitespace carries no structural indent).
fn line_indent(text: &str) -> Option<usize> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text.len() - text.trim_start().len())
    }
}

/// A header line that only continues the construct above it rather than opening
/// it: a lone brace (`{`), or a signature tail that leads with a closing
/// delimiter (`) -> T {`). Such a line's real definition — the `fn`/`def`/type
/// signature — lives on the preceding line(s), so the block start climbs past
/// it. A normal header (`fn foo() {`, `def handler():`) leads with a word and is
/// not a continuation.
fn is_continuation_line(text: &str) -> bool {
    let trimmed = text.trim();
    match trimmed.chars().next() {
        None => false,
        Some(first) => ")]}".contains(first) || trimmed.chars().all(|c| "{([ \t".contains(c)),
    }
}

/// A decorator, attribute, or comment/doc line that annotates the definition
/// directly below it: `@decorator`, `#[attr]`, `///`/`//`/`/* */`/`*` doc, `#`
/// (Python/shell/Ruby), `--` (SQL/Lua), `;;` (Lisp/asm). Absorbed upward into a
/// block so the chunk carries what the code *is*, not just its body.
fn is_annotation_line(text: &str) -> bool {
    let trimmed = text.trim_start();
    const PREFIXES: &[&str] = &["@", "#[", "///", "//", "/*", "*/", "*", "#", "--", ";;"];
    PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix))
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

/// One chunk recalled by the persistent dense index: a block that the question
/// resembles by meaning, with its cosine similarity. Its file is returned
/// alongside (see [`DenseRecall`]) so retrieval can chunk and, for the analyze
/// path, deliver it even when the lexical search never touched that file.
#[derive(Debug, Clone)]
pub struct DenseHit {
    pub path: String,
    pub range: LineRange,
    pub similarity: f32,
}

/// The dense index's answer to a question: the top recalled chunks and the
/// files they came from (deduplicated), ready to fuse with the lexical ranking.
#[derive(Debug, Clone, Default)]
pub struct DenseRecall {
    pub documents: Vec<Document>,
    pub hits: Vec<DenseHit>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrieveResult {
    pub version: u8,
    pub query_terms: Vec<String>,
    pub chunks: Vec<RetrievedChunk>,
    pub estimated_tokens: usize,
    pub truncated: bool,
    /// Whole size of the files retrieval loaded, before chunking — the token
    /// cost the caller would have paid reading them entire. Kept out of the
    /// output contract (`skip`); it feeds the savings telemetry only.
    #[serde(skip)]
    pub baseline_bytes: usize,
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
    /// Copy-paste command that prints this finding's exact source lines, so
    /// the host can verify the answer against the real file. Populated during
    /// validation; never trusted from the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_hint: Option<String>,
}

/// A finding the worker produced that failed source validation, kept for
/// disclosure instead of silently dropped or failing the whole response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFinding {
    #[serde(flatten)]
    pub finding: Finding,
    /// Stable reason token: `unknown path`, `summary too long`, or
    /// `line range not in retrieved context`.
    pub reason: String,
}

/// One locally changed file in the working tree: the changed line ranges in
/// the current (post-change) version, plus the text of the added lines.
/// `whole_file` is true only for an untracked (brand-new) file, where no line
/// restriction applies; a tracked file with empty `hunks` is pure deletions
/// and must keep every hit unboosted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub hunks: Vec<LineRange>,
    /// True only for untracked files: the entire file is new content.
    pub whole_file: bool,
    pub changed_lines: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub version: u8,
    pub answer: String,
    pub findings: Vec<Finding>,
    /// Worker findings that failed source validation, disclosed rather than
    /// shown: a host reading `findings` never sees an unverified reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_findings: Vec<DroppedFinding>,
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
    /// Whole size of the files a deterministic retrieve loaded — the baseline
    /// the caller would have read without bounded retrieval. Zero (and `default`
    /// on read) for other operations and legacy records; paired with
    /// `delivered_tokens`, it lets the summary report real token savings.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub baseline_bytes: usize,
    /// Tokens the retrieve actually returned. Zero for non-retrieve operations
    /// and legacy records.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub delivered_tokens: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
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

#[cfg(test)]
mod tests {
    use super::{Document, LineRange};

    fn document(source: &str) -> Document {
        let lines = source.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        let line_count = lines.len();
        Document {
            path: "x.rs".to_owned(),
            bytes: source.len(),
            line_count,
            lines,
            numbered_content: String::new(),
            allowed_ranges: Vec::new(),
        }
    }

    #[test]
    fn enclosing_block_snaps_hit_to_its_function_body() {
        // fn header at indent 0, body at indent 4, closing brace back at 0.
        let doc = document(
            "fn outer() {\n    let a = 1;\n    let b = 2;\n    call(a, b);\n}\nfn other() {}\n",
        );
        // Hit on `call(a, b);` (line 4) expands to the fn header..last body line,
        // never crossing into `other`.
        let range = doc.enclosing_block(4, 48).unwrap();
        assert_eq!(
            range,
            LineRange {
                start_line: 1,
                end_line: 4
            }
        );
    }

    #[test]
    fn enclosing_block_collapses_two_hits_in_one_function_to_the_same_range() {
        let doc = document("fn outer() {\n    let a = 1;\n    let b = 2;\n    call(a, b);\n}\n");
        assert_eq!(doc.enclosing_block(2, 48), doc.enclosing_block(4, 48));
    }

    #[test]
    fn enclosing_block_bails_on_top_level_hit() {
        // A hit on a line with no less-indented ancestor keeps the fixed window.
        let doc = document("use crate::foo;\nuse crate::bar;\n");
        assert_eq!(doc.enclosing_block(1, 48), None);
    }

    #[test]
    fn enclosing_block_bails_when_block_exceeds_max_span() {
        let mut source = String::from("fn big() {\n");
        for index in 0..60 {
            source.push_str(&format!("    let v{index} = {index};\n"));
        }
        source.push_str("}\n");
        let doc = document(&source);
        assert_eq!(doc.enclosing_block(30, 48), None);
    }

    #[test]
    fn enclosing_block_keeps_the_signature_in_allman_brace_style() {
        // C/Go/Java/C# style: the `{` sits on its own line, so the naive header
        // is the brace and the signature above it must be pulled in.
        let doc = document("int foo(int x)\n{\n    return x;\n}\n");
        let range = doc.enclosing_block(3, 48).unwrap();
        assert_eq!(
            range,
            LineRange {
                start_line: 1,
                end_line: 3
            }
        );
    }

    #[test]
    fn enclosing_block_keeps_a_multi_line_signature() {
        // The `) -> T {` continuation is not the definition line; the block
        // climbs to `fn foo(` and includes the parameter lines.
        let doc = document("fn foo(\n    a: i32,\n) -> T {\n    body(a);\n}\n");
        let range = doc.enclosing_block(4, 48).unwrap();
        assert_eq!(range.start_line, 1);
    }

    #[test]
    fn enclosing_block_absorbs_a_python_decorator() {
        let doc = document("@app.route(\"/\")\ndef handler():\n    return ok()\n");
        let range = doc.enclosing_block(3, 48).unwrap();
        assert_eq!(range.start_line, 1);
    }

    #[test]
    fn enclosing_block_absorbs_rust_attribute_and_doc() {
        let doc =
            document("/// Does the thing.\n#[test]\nfn checks() {\n    assert!(work());\n}\n");
        let range = doc.enclosing_block(4, 48).unwrap();
        assert_eq!(range.start_line, 1);
    }

    #[test]
    fn enclosing_block_does_not_absorb_across_a_blank_line() {
        // An unrelated statement two lines up, separated by a blank, is not part
        // of the definition and must stay out of the chunk.
        let doc = document("let unrelated = 1;\n\ndef handler():\n    return ok()\n");
        let range = doc.enclosing_block(4, 48).unwrap();
        assert_eq!(range.start_line, 3);
    }

    #[test]
    fn trim_trivial_strips_blank_and_delimiter_edges() {
        let doc = document("{\n\n    real();\n}\n");
        assert_eq!(
            doc.trim_trivial(LineRange {
                start_line: 1,
                end_line: 4
            }),
            LineRange {
                start_line: 3,
                end_line: 3
            }
        );
    }

    #[test]
    fn trim_trivial_never_empties_an_all_trivial_range() {
        let doc = document("{\n}\n");
        let trimmed = doc.trim_trivial(LineRange {
            start_line: 1,
            end_line: 2,
        });
        assert_eq!(trimmed.start_line, trimmed.end_line);
    }
}
