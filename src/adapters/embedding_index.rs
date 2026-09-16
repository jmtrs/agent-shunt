use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    adapters::{openai_compatible::cosine, ripgrep},
    application::ports::{DenseIndex, Embedder, StructureResolver},
    domain::{DenseHit, DenseRecall, Document, Limits, LineRange},
};

/// Persistent, whole-repository dense index. It lists the repo's source files,
/// chunks each into blocks (AST definitions or fixed windows via the resolver),
/// embeds every block once — caching vectors on disk, keyed by file content and
/// embedding model — and answers a question with the top blocks by cosine
/// similarity. This recalls relevant code the lexical search never matched, for
/// retrieval to fuse with the lexical ranking. Only the opt-in `--semantic` path
/// builds one; the default `retrieve` never constructs it.
pub struct EmbeddingIndex<'a> {
    embedder: &'a dyn Embedder,
    resolver: &'a dyn StructureResolver,
    cache_dir: PathBuf,
    model_tag: String,
    top_k: usize,
    max_block_lines: usize,
    max_files: usize,
    max_chunks: usize,
}

/// Cache-format version. Bumped whenever the chunking that produces the cached
/// blocks changes, so a new binary never reuses vectors computed under different
/// block boundaries even when a file's content is unchanged.
const INDEX_VERSION: u32 = 1;

/// One embedded block: the file it belongs to and its line span.
struct Block {
    file: usize,
    range: LineRange,
}

/// On-disk cache of one file's block vectors, invalidated by the content hash in
/// its filename and the embedding model recorded inside.
#[derive(Serialize, Deserialize)]
struct FileCache {
    model: String,
    blocks: Vec<(usize, usize)>,
    vectors: Vec<Vec<f32>>,
}

impl<'a> EmbeddingIndex<'a> {
    pub fn new(
        embedder: &'a dyn Embedder,
        resolver: &'a dyn StructureResolver,
        cache_dir: PathBuf,
        model_tag: &str,
        top_k: usize,
        max_block_lines: usize,
    ) -> Self {
        Self {
            embedder,
            resolver,
            cache_dir,
            model_tag: model_tag.to_owned(),
            top_k,
            max_block_lines,
            max_files: 4_000,
            max_chunks: 6_000,
        }
    }

    /// Lists the repository's candidate source files with ripgrep, honoring the
    /// caller globs and the built-in exclusions (`node_modules`, `.git`, `.env`,
    /// lockfiles). Binary-by-extension paths are dropped up front.
    fn list_files(&self, root: &Path, globs: &[String]) -> Result<Vec<PathBuf>> {
        let mut command = Command::new("rg");
        command.current_dir(root).args(["--files", "--no-messages"]);
        ripgrep::apply_globs(&mut command, globs);
        let output = command
            .output()
            .context("failed to list files with ripgrep")?;
        if !output.status.success() && output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        let mut files = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty() && !ripgrep::has_binary_extension(line))
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        files.sort();
        files.truncate(self.max_files);
        Ok(files)
    }

    fn cache_path(&self, content_key: u64) -> PathBuf {
        self.cache_dir.join(format!("{content_key:016x}.json"))
    }

    /// Blocks and their vectors for one file, from the disk cache when the
    /// content and model match, otherwise embedded fresh and written back.
    fn file_blocks(
        &self,
        lines: &[String],
        rel_path: &Path,
        content: &str,
    ) -> Result<(Vec<LineRange>, Vec<Vec<f32>>)> {
        let mut hasher = DefaultHasher::new();
        INDEX_VERSION.hash(&mut hasher);
        self.model_tag.hash(&mut hasher);
        content.hash(&mut hasher);
        let key = hasher.finish();
        let path = self.cache_path(key);
        if let Ok(bytes) = fs::read(&path)
            && let Ok(cache) = serde_json::from_slice::<FileCache>(&bytes)
            && cache.model == self.model_tag
            && cache.blocks.len() == cache.vectors.len()
        {
            let ranges = cache
                .blocks
                .into_iter()
                .map(|(start_line, end_line)| LineRange {
                    start_line,
                    end_line,
                })
                .collect();
            return Ok((ranges, cache.vectors));
        }
        let ranges = self
            .resolver
            .all_blocks(rel_path, lines, self.max_block_lines);
        if ranges.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let texts = ranges
            .iter()
            .map(|range| block_text(lines, *range))
            .collect::<Vec<_>>();
        let vectors = self.embedder.embed(&texts)?;
        let entry = FileCache {
            model: self.model_tag.clone(),
            blocks: ranges
                .iter()
                .map(|range| (range.start_line, range.end_line))
                .collect(),
            vectors: vectors.clone(),
        };
        // A cache write failure is non-fatal: the vectors are already in hand,
        // and the next run simply re-embeds the file.
        if fs::create_dir_all(&self.cache_dir).is_ok()
            && let Ok(serialized) = serde_json::to_vec(&entry)
        {
            let _ = fs::write(&path, serialized);
        }
        Ok((ranges, vectors))
    }
}

impl DenseIndex for EmbeddingIndex<'_> {
    fn recall(
        &self,
        question: &str,
        root: &Path,
        globs: &[String],
        limits: &Limits,
    ) -> Result<DenseRecall> {
        let files = self.list_files(root, globs)?;
        let mut documents: Vec<Document> = Vec::new();
        let mut blocks: Vec<Block> = Vec::new();
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for rel_path in &files {
            if blocks.len() >= self.max_chunks {
                break;
            }
            let absolute = root.join(rel_path);
            let Ok(content) = fs::read_to_string(&absolute) else {
                continue;
            };
            if content.len() > limits.max_file_bytes {
                continue;
            }
            let lines = content.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
            let (ranges, file_vectors) = self.file_blocks(&lines, rel_path, &content)?;
            if ranges.is_empty() || ranges.len() != file_vectors.len() {
                continue;
            }
            let file_index = documents.len();
            documents.push(Document {
                path: rel_path.to_string_lossy().into_owned(),
                bytes: content.len(),
                line_count: lines.len(),
                lines,
                numbered_content: String::new(),
                allowed_ranges: Vec::new(),
            });
            for (range, vector) in ranges.into_iter().zip(file_vectors) {
                blocks.push(Block {
                    file: file_index,
                    range,
                });
                vectors.push(vector);
            }
        }
        if blocks.is_empty() {
            return Ok(DenseRecall::default());
        }
        let question_vectors = self.embedder.embed(&[question.to_owned()])?;
        let query = question_vectors
            .first()
            .ok_or_else(|| anyhow::anyhow!("embedding endpoint returned no question vector"))?;
        let mut scored = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (index, cosine(query, &vectors[index]), block))
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        scored.truncate(self.top_k);
        // Keep only the files a surviving hit refers to, and remap indices.
        let mut kept: Vec<Option<usize>> = vec![None; documents.len()];
        let mut kept_documents = Vec::new();
        let mut hits = Vec::new();
        for (_, similarity, block) in scored {
            let new_index = *kept[block.file].get_or_insert_with(|| {
                kept_documents.push(documents[block.file].clone());
                kept_documents.len() - 1
            });
            hits.push(DenseHit {
                path: kept_documents[new_index].path.clone(),
                range: block.range,
                similarity,
            });
        }
        Ok(DenseRecall {
            documents: kept_documents,
            hits,
        })
    }
}

/// The raw source of a block, without line-number prefixes, for embedding.
fn block_text(lines: &[String], range: LineRange) -> String {
    let start = range.start_line.saturating_sub(1);
    let end = range.end_line.min(lines.len());
    lines[start..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::resolver::HeuristicResolver;

    struct StubEmbedder;
    impl Embedder for StubEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            // Two-dimensional toy embedding: axis 0 fires on "auth", axis 1 on
            // "payment", so cosine ranks by which topic a text mentions.
            Ok(texts
                .iter()
                .map(|text| {
                    vec![
                        text.matches("auth").count() as f32 + 0.01,
                        text.matches("payment").count() as f32,
                    ]
                })
                .collect())
        }
    }

    #[test]
    fn recall_ranks_the_topically_matching_block_first() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("auth.rs"),
            "fn login() {\n    let auth = check_auth();\n    auth\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("pay.rs"),
            "fn charge() {\n    let payment = take_payment();\n    payment\n}\n",
        )
        .unwrap();
        let embedder = StubEmbedder;
        let resolver = HeuristicResolver;
        let index = EmbeddingIndex::new(
            &embedder,
            &resolver,
            root.path().join(".cache"),
            "stub-model",
            5,
            48,
        );
        let recall = index
            .recall(
                "where is auth checked",
                root.path(),
                &[],
                &Limits::default(),
            )
            .unwrap();
        assert!(!recall.hits.is_empty());
        // The auth block outranks the payment block for an auth question.
        assert_eq!(recall.hits[0].path, "auth.rs");
        // The recalled file is returned so retrieval can deliver it.
        assert!(recall.documents.iter().any(|doc| doc.path == "auth.rs"));
    }

    #[test]
    fn second_run_reads_vectors_from_the_disk_cache() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("a.rs"),
            "fn f() {\n    let auth = 1;\n    auth\n}\n",
        )
        .unwrap();
        let cache = root.path().join(".cache");
        let resolver = HeuristicResolver;

        struct CountingEmbedder {
            calls: std::cell::RefCell<usize>,
        }
        impl Embedder for CountingEmbedder {
            fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                *self.calls.borrow_mut() += 1;
                Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
            }
        }

        let embedder = CountingEmbedder {
            calls: std::cell::RefCell::new(0),
        };
        let build = || {
            EmbeddingIndex::new(&embedder, &resolver, cache.clone(), "m", 3, 48)
                .recall("auth", root.path(), &[], &Limits::default())
                .unwrap()
        };
        build();
        let after_first = *embedder.calls.borrow();
        build();
        let after_second = *embedder.calls.borrow();
        // The second run still embeds the question, but the file's block vectors
        // come from the cache, so it makes fewer calls than a cold first run.
        assert!(
            after_second - after_first < after_first,
            "cache did not reduce embedding calls: {after_first} then {}",
            after_second - after_first
        );
    }
}
