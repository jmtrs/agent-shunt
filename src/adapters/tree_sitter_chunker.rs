use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::Path,
    sync::Mutex,
};

use tree_sitter::{Language, Node, Parser};

use crate::{application::ports::StructureResolver, domain::LineRange};

/// Structure-aware [`StructureResolver`] backed by tree-sitter. It snaps a hit
/// to its enclosing *definition* — the function, method, or class that contains
/// it, together with its decorators, attributes, and doc-comments — instead of
/// a fixed line window. For files in a language it does not parse (or when no
/// definition fits `max_span`), it falls back to the language-agnostic
/// indentation heuristic, so behaviour never regresses below the plain resolver.
///
/// Parsing is memoised per file content: retrieval snaps several hits from the
/// same file, so the definition spans are computed once and reused.
#[derive(Default)]
pub struct AstResolver {
    cache: Mutex<HashMap<u64, Vec<Span>>>,
}

/// A definition's 1-indexed line span, decorators and doc-comments included.
#[derive(Clone, Copy)]
struct Span {
    start_line: usize,
    end_line: usize,
}

impl AstResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tightest cached definition span that contains `line` and fits within
    /// `max_span` lines, or `None` when the language is unsupported, parsing
    /// yields nothing, or every containing definition is too large.
    fn ast_block(
        &self,
        path: &Path,
        lines: &[String],
        line: usize,
        max_span: usize,
    ) -> Option<LineRange> {
        let language = language_for(path)?;
        let source = lines.join("\n");
        let key = content_key(path, &source);
        let spans = {
            let mut cache = self.cache.lock().ok()?;
            cache
                .entry(key)
                .or_insert_with(|| definition_spans(language, &source))
                .clone()
        };
        spans
            .iter()
            .filter(|span| {
                span.start_line <= line
                    && line <= span.end_line
                    && span.end_line - span.start_line < max_span
            })
            // Tightest enclosing definition: the method, not the whole class.
            .min_by_key(|span| (span.end_line - span.start_line, span.start_line))
            .map(|span| LineRange {
                start_line: span.start_line,
                end_line: span.end_line,
            })
    }
}

impl StructureResolver for AstResolver {
    fn enclosing_block(
        &self,
        path: &Path,
        lines: &[String],
        line: usize,
        max_span: usize,
    ) -> Option<LineRange> {
        self.ast_block(path, lines, line, max_span)
            .or_else(|| crate::domain::enclosing_block(lines, line, max_span))
    }
}

fn content_key(path: &Path, source: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    source.hash(&mut hasher);
    hasher.finish()
}

/// Maps a file extension to a tree-sitter language. Unknown extensions return
/// `None`, and the resolver falls back to the heuristic.
fn language_for(path: &Path) -> Option<Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let language = match extension.as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" | "pyi" => tree_sitter_python::LANGUAGE.into(),
        "js" | "mjs" | "cjs" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => tree_sitter_cpp::LANGUAGE.into(),
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "sh" | "bash" => tree_sitter_bash::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        _ => return None,
    };
    Some(language)
}

/// Parses `source` and collects the line span of every definition-like node,
/// each extended upward to include its decorators, attributes, and doc-comments.
fn definition_spans(language: Language, source: &str) -> Vec<Span> {
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    let mut cursor = tree.walk();
    // Iterative pre-order traversal over every named node.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_named() && is_definition_kind(node.kind()) {
            let start_row = extended_start_row(node);
            let end_row = node.end_position().row;
            if end_row >= start_row {
                spans.push(Span {
                    start_line: start_row + 1,
                    end_line: end_row + 1,
                });
            }
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    spans
}

/// The definition's start row (0-indexed), climbing through a decoration
/// wrapper (Python's `decorated_definition`) and absorbing the run of comment,
/// attribute, or annotation siblings on the lines directly above it.
fn extended_start_row(node: Node) -> usize {
    let mut top = node;
    while let Some(parent) = top.parent() {
        if is_decoration_wrapper(parent.kind()) {
            top = parent;
        } else {
            break;
        }
    }
    let mut start_row = top.start_position().row;
    let mut sibling = top.prev_sibling();
    while let Some(previous) = sibling {
        // A comment node spans its trailing newline, so its `end` lands at
        // column 0 of the following row; the last row it actually covers is one
        // above that. Absorb the annotation only when that row abuts the current
        // start with no blank line between.
        let end = previous.end_position();
        let last_row = if end.column == 0 {
            end.row.saturating_sub(1)
        } else {
            end.row
        };
        if is_annotation_kind(previous.kind()) && last_row + 1 == start_row {
            start_row = previous.start_position().row;
            sibling = previous.prev_sibling();
        } else {
            break;
        }
    }
    start_row
}

/// Node kinds that name a self-contained definition worth returning whole,
/// matched by substring so one list spans every grammar (`function_item`,
/// `function_definition`, `method_declaration`, `class_declaration`, ...).
fn is_definition_kind(kind: &str) -> bool {
    const MARKERS: &[&str] = &[
        "function",
        "method",
        "class",
        "struct",
        "impl",
        "trait",
        "interface",
        "enum",
        "constructor",
        "namespace",
        "module",
    ];
    // Exclude type-level uses like `function_type` that merely mention the word.
    if kind.ends_with("_type") {
        return false;
    }
    MARKERS.iter().any(|marker| kind.contains(marker))
}

fn is_decoration_wrapper(kind: &str) -> bool {
    kind.contains("decorat")
}

fn is_annotation_kind(kind: &str) -> bool {
    kind.contains("comment")
        || kind.contains("attribute")
        || kind.contains("annotation")
        || kind.contains("decorator")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<String> {
        source.lines().map(ToOwned::to_owned).collect()
    }

    #[test]
    fn rust_hit_expands_to_the_whole_function_with_attribute_and_doc() {
        let resolver = AstResolver::new();
        let source = "\
/// Doc.
#[inline]
fn compute(x: i32) -> i32 {
    let y = x + 1;
    y * 2
}
";
        let range = resolver
            .enclosing_block(Path::new("a.rs"), &lines(source), 4, 48)
            .unwrap();
        // Attribute and doc are pulled in; body end included.
        assert_eq!(range.start_line, 1);
        assert_eq!(range.end_line, 6);
    }

    #[test]
    fn python_hit_includes_the_decorator() {
        let resolver = AstResolver::new();
        let source = "\
@app.route(\"/\")
def handler():
    return ok()
";
        let range = resolver
            .enclosing_block(Path::new("a.py"), &lines(source), 3, 48)
            .unwrap();
        assert_eq!(range.start_line, 1);
    }

    #[test]
    fn go_allman_hit_keeps_the_signature() {
        let resolver = AstResolver::new();
        let source = "\
package main

func Add(a int, b int) int {
    sum := a + b
    return sum
}
";
        let range = resolver
            .enclosing_block(Path::new("a.go"), &lines(source), 4, 48)
            .unwrap();
        assert_eq!(range.start_line, 3);
        assert_eq!(range.end_line, 6);
    }

    #[test]
    fn tightest_definition_wins_for_a_method_inside_a_class() {
        let resolver = AstResolver::new();
        let source = "\
class Widget:
    def render(self):
        return draw(self)
";
        let range = resolver
            .enclosing_block(Path::new("a.py"), &lines(source), 3, 48)
            .unwrap();
        // The method, not the enclosing class.
        assert_eq!(range.start_line, 2);
    }

    #[test]
    fn unsupported_extension_falls_back_to_the_heuristic() {
        let resolver = AstResolver::new();
        // A made-up extension has no grammar; the indentation heuristic still
        // snaps the hit to its block.
        let source = "outer:\n    inner_call()\n";
        let range = resolver.enclosing_block(Path::new("a.zzz"), &lines(source), 2, 48);
        assert_eq!(range.unwrap().start_line, 1);
    }
}
