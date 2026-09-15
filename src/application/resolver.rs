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
}
