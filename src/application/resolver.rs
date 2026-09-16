use std::path::Path;

use crate::{application::ports::StructureResolver, domain::LineRange};

/// The default, dependency-free [`StructureResolver`]: the language-agnostic
/// indentation heuristic in [`crate::domain::enclosing_block`]. Used directly by
/// tests and by the plain `execute` entry point, and as the fallback the
/// AST-backed resolver delegates to for unsupported languages.
#[derive(Default)]
pub struct HeuristicResolver;

impl StructureResolver for HeuristicResolver {
    fn enclosing_block(
        &self,
        _path: &Path,
        lines: &[String],
        line: usize,
        max_span: usize,
    ) -> Option<LineRange> {
        crate::domain::enclosing_block(lines, line, max_span)
    }

    fn all_blocks(&self, _path: &Path, lines: &[String], max_span: usize) -> Vec<LineRange> {
        fixed_windows(lines, max_span)
    }
}

/// Non-overlapping fixed-size line windows covering a file, skipping windows
/// that are entirely blank. The dependency-free chunker for whole-file indexing
/// when no grammar applies. Window size is `max_span`, clamped to a sane floor.
pub(crate) fn fixed_windows(lines: &[String], max_span: usize) -> Vec<LineRange> {
    let window = max_span.clamp(1, 80);
    let mut blocks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + window).min(lines.len());
        if lines[start..end].iter().any(|line| !line.trim().is_empty()) {
            blocks.push(LineRange {
                start_line: start + 1,
                end_line: end,
            });
        }
        start = end;
    }
    blocks
}
