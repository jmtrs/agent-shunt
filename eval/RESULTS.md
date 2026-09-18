# Retrieval benchmark results

Context-savings baseline: 2026-09-17  
Semantic retrieval update: 2026-09-18

These results measure source-context efficiency and retrieval quality for `agent-shunt` lexical retrieval across 50 human-authored architecture and implementation questions over five pinned open-source repositories in Rust, Python, TypeScript, Go, and Java.

Three baseline comparisons are reported:

1. **Same selected files in full**: a deliberately conservative baseline that is given `agent-shunt`'s selected files for free and reads those files completely.
2. **Plain ripgrep + whole-file reads**: a reproducible search workflow that receives the same preprocessed query terms as `agent-shunt`, ranks matching files, and reads the top 1, 3, or 5 files completely.
3. **Plain ripgrep + targeted reads**: the same ripgrep ranking followed by either a fixed context window or a bounded enclosing-symbol read, under the same 1,200-token budget as `agent-shunt`.

All token counts below are **estimated context tokens**, not provider billing tokens.

## Headline

Across the current 50-case corpus, `agent-shunt` delivered **51,835 estimated context tokens**.

Against reading the exact same selected files in full, the baseline required **905,462** estimated tokens. That is an aggregate **94.28% context reduction**, or **17.47x less context**.

Against the more operational `rg + whole-file reads` baseline:

| Comparison | agent-shunt context | Baseline context | Reduction | Compression |
| --- | ---: | ---: | ---: | ---: |
| `rg` top 1 file in full | 51,835 | 658,290 | **92.13%** | **12.70x** |
| `rg` top 3 files in full | 51,835 | 1,590,547 | **96.74%** | **30.68x** |
| `rg` top 5 files in full | 51,835 | 2,432,356 | **97.87%** | **46.92x** |

The file-level retrieval comparison is also favorable in aggregate:

| System | File-Hit@1 | File-Hit@3 | File-Hit@5 | File MRR |
| --- | ---: | ---: | ---: | ---: |
| `agent-shunt` lexical | **50%** | **76%** | **78%** | **0.611** |
| plain `rg` ranking | 28% | 62% | 74% | 0.489 |

These are separate from `agent-shunt`'s chunk-level quality metrics. Chunk-level lexical quality remains **Hit@1 50%**, **Hit@3 74%**, **Hit@5 78%**, and **MRR 0.602** under the 1,200-token budget.

## Budgeted targeted-navigation baseline

The whole-file baseline measures context compression, but a competent repository-navigation workflow usually does not open every matching file in full. The stronger deterministic comparison therefore keeps plain `rg` for discovery and gives it the **same 1,200-token context budget** as `agent-shunt`.

Two targeted variants are measured:

- **`rg + window`**: read the configured ±8-line context around ranked matches.
- **`rg + symbol`**: expand each ranked match to its bounded enclosing function/method/class when available, otherwise fall back to the same fixed window.

Overlapping reads are deduplicated. Neither variant uses filename/path boosts, IDF weighting, MMR, PRF, semantic retrieval, or relevance pruning. The symbol variant shares only the structural resolver after plain `rg` has already chosen the hit.

| Repository | agent Hit@5 | agent MRR | agent tokens | `rg + window` Hit@5 | window MRR | window tokens | `rg + symbol` Hit@5 | symbol MRR | symbol tokens |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | **90%** | **0.587** | **10,066** | 20% | 0.133 | 11,609 | 30% | 0.153 | 11,801 |
| Flask | **70%** | **0.633** | **11,415** | 20% | 0.200 | 11,418 | 20% | 0.200 | 11,657 |
| Hono | **60%** | **0.417** | **9,973** | 40% | 0.320 | 11,634 | 30% | 0.313 | 11,826 |
| Cobra | **100%** | **0.925** | **11,039** | 50% | 0.500 | 11,571 | 70% | 0.545 | 11,731 |
| Gson | **70%** | **0.450** | **9,342** | 40% | 0.283 | 11,668 | 30% | 0.250 | 11,687 |
| **Aggregate, 50 cases** | **78%** | **0.602** | **51,835** | 34% | 0.287 | 57,900 | 36% | 0.292 | 58,702 |

At the same nominal context ceiling, aggregate chunk quality is:

| Strategy | Hit@1 | Hit@3 | Hit@5 | MRR | Avg. context/query |
| --- | ---: | ---: | ---: | ---: | ---: |
| `agent-shunt` lexical | **50%** | **74%** | **78%** | **0.602** | **1,036.7** |
| `rg + window` | 26% | 32% | 34% | 0.287 | 1,158.0 |
| `rg + symbol` | 26% | 30% | 36% | 0.292 | 1,174.0 |

This is a more meaningful result than the whole-file compression ratio: the comparison no longer relies on the baseline wasting context by opening entire files. On this corpus, the production lexical pipeline retrieves substantially more acceptable evidence while also delivering about **10.5% less estimated context than `rg + window`** and **11.7% less than `rg + symbol`**.

The result still does **not** prove superiority to a coding agent. A real agent may reformulate queries, follow references, use an LSP, inspect repository maps, or perform multiple adaptive search/read rounds. This benchmark isolates one deterministic question: whether agent-shunt's retrieval pipeline improves on a strong one-pass ripgrep navigation workflow under the same evidence budget.

The machine-readable snapshot is [`eval/results/2026-09-18-targeted-rg.json`](results/2026-09-18-targeted-rg.json), validated by the `Ripgrep navigation baselines` workflow across all five pinned repositories.

## Semantic retrieval quality

The opt-in local semantic path uses the production confidence-gated fusion policy with `bge-small-en-v1.5` and dense top-k 24. It was evaluated against lexical retrieval on the same 50 human-authored questions, same pinned repositories, same AST chunking and MMR pipeline, and the same 1,200-token evidence budget.

| Repository | Lexical Hit@1 | Hit@3 | Hit@5 | Lexical MRR | Semantic Hit@1 | Hit@3 | Hit@5 | Semantic MRR |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | 40% | **80%** | 90% | 0.587 | 40% | 70% | 90% | **0.595** |
| Flask | 60% | 70% | 70% | 0.633 | 60% | **90%** | **90%** | **0.750** |
| Hono | 30% | 60% | 60% | 0.417 | 30% | **80%** | **80%** | **0.500** |
| Cobra | 90% | 90% | 100% | 0.925 | 90% | **100%** | 100% | **0.950** |
| Gson | 30% | 70% | 70% | 0.450 | 30% | **80%** | **80%** | **0.517** |
| **Aggregate, 50 cases** | **50%** | 74% | 78% | 0.602 | **50%** | **84%** | **88%** | **0.662** |

Aggregate Hit@1 is unchanged. Semantic retrieval adds **10 percentage points at Hit@3 and Hit@5** and improves MRR by about **0.060**. Average delivered context is **1,052.1 estimated tokens/query** for semantic retrieval versus **1,036.7** for lexical retrieval, a ~1.5% increase while remaining under the same 1,200-token cap.

The result is intentionally not presented as a universal win. On ripgrep, `gitignore-matching` moves from rank 3 to rank 4 because semantic recall introduces `crates/ignore/src/incremental.rs`; `file-type-globs` moves from rank 3 to rank 2. As a result, ripgrep Hit@3 falls from 80% to 70%, while Hit@5 remains 90% and MRR rises slightly from 0.587 to 0.595. This trade-off is kept visible rather than tuning specifically to the benchmark case.

The production semantic policy is bounded:

1. lexical scores and the lexical #1 result remain authoritative;
2. at most one semantic region is admitted;
3. the leading semantic file must beat the next distinct semantic file by a relative confidence margin;
4. semantic recall does not fall through to a lower-ranked file when the validated head cannot contribute;
5. all evidence still competes under the same token budget.

The local FastEmbed backend uses a bounded batch size of 32. This was validated against ripgrep's ~246 KiB `crates/core/flags/defs.rs`, which previously caused the hosted evaluation runner to be terminated under the larger automatic batch.

Reproduce one corpus:

```bash
cargo run --release --features local-embed --example semantic_eval -- \
  eval/external/<corpus>.json \
  --root /path/to/pinned/repository \
  --json
```

The `Semantic retrieval evaluation` GitHub Actions workflow runs the same evaluator across all five pinned corpora and publishes each JSON report as an artifact.

The published machine-readable summary is [`eval/results/2026-09-18-semantic.json`](results/2026-09-18-semantic.json). It records the final validation run IDs, per-corpus metrics, aggregate metrics, model settings, and caveats.

## 1. Same selected files in full

This comparison isolates the value of focused chunk selection after file discovery has already succeeded. The baseline receives perfect file selection for free: for each question it reads only the distinct files that actually contributed chunks selected by `agent-shunt`.

| Repository | Language | Commit | Chunk Hit@1 | Hit@3 | Hit@5 | Chunk MRR | Delivered tokens | Same files in full | Reduction | Compression | Median case reduction | p95 delivered | p95 full files |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | Rust | `3fce3b5bb023` | 40% | 80% | 90% | 0.587 | 10,066 | 406,114 | **97.5%** | **40.35x** | 97.0% | 1,200 | 117,616 |
| Flask | Python | `d73fa1cdcbd8` | 60% | 70% | 70% | 0.633 | 11,415 | 149,620 | **92.4%** | **13.11x** | 89.1% | 1,189 | 36,171 |
| Hono | TypeScript | `098e11912ab2` | 30% | 60% | 60% | 0.417 | 9,973 | 86,385 | **88.5%** | **8.66x** | 85.1% | 1,190 | 31,702 |
| Cobra | Go | `adbc8813901b` | 90% | 90% | 100% | 0.925 | 11,039 | 124,082 | **91.1%** | **11.24x** | 92.1% | 1,191 | 27,427 |
| Gson | Java | `854c8255b625` | 30% | 70% | 70% | 0.450 | 9,342 | 139,261 | **93.3%** | **14.91x** | 92.4% | 1,189 | 36,941 |
| **Aggregate, 50 cases** | 5 languages | pinned below | **50%** | **74%** | **78%** | **0.602** | **51,835** | **905,462** | **94.28%** | **17.47x** | n/a | n/a | n/a |

Average context per question is **1,037 estimated tokens** from `agent-shunt` versus **18,109 estimated tokens** for reading those same selected files in full.

### Baseline definition

For every question:

1. Run the production lexical retrieval path with the corpus settings.
2. Record the chunks that survive ranking, chunk selection, MMR, relevance filtering, and the 1,200-token budget.
3. Collect the distinct source files represented by those chunks.
4. Reload only those exact files through `SecureFilesystem`.
5. Compare the token estimate for the selected chunks with the token estimate for those same files in full.

This baseline intentionally does **not** measure against the entire repository. It answers a narrower and harder question:

> If another system somehow knew exactly which files `agent-shunt` selected, how much context would focused chunk retrieval still save compared with reading those files completely?

## 2. Plain ripgrep + whole-file reads

This baseline models a simple search-first coding workflow without claiming equivalence to any proprietary coding agent.

To make the comparison conservative, plain `rg` receives the **same preprocessed query terms** generated by `agent-shunt`; it is not forced to search the raw natural-language question.

For each question:

1. Build an OR search from the same escaped query terms.
2. Run plain `rg` against the same pinned checkout and corpus globs.
3. Rank matching files by distinct query-term coverage, then matching-line count, then path for deterministic tie-breaking.
4. Read the top 1, 3, or 5 ranked files in full through `SecureFilesystem`.
5. Estimate context using the same numbered source representation and conservative token estimator used for `agent-shunt`.

The baseline does not use `agent-shunt` filename/path boosts, IDF weighting, AST chunking, MMR, PRF, relevance pruning, or chunk budgeting.

### Results by repository at top 5

| Repository | agent File-Hit@5 | `rg` File-Hit@5 | agent File MRR | `rg` File MRR | agent context | `rg` top-5 full-file context | Context reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | **90%** | 40% | **0.587** | 0.289 | 10,066 | 936,411 | **98.93%** |
| Flask | 70% | 70% | **0.650** | 0.448 | 11,415 | 592,814 | **98.07%** |
| Hono | 60% | **90%** | 0.417 | **0.513** | 9,973 | 220,447 | **95.48%** |
| Cobra | **100%** | 90% | **0.950** | 0.768 | 11,039 | 348,401 | **96.83%** |
| Gson | 70% | **80%** | **0.450** | 0.427 | 9,342 | 334,283 | **97.21%** |
| **Aggregate, 50 cases** | **78%** | 74% | **0.611** | 0.489 | **51,835** | **2,432,356** | **97.87%** |

The per-repository results are intentionally not cherry-picked. Plain `rg` has higher File-Hit@5 on **Hono (90% vs 60%)** and **Gson (80% vs 70%)**. The aggregate result still favors `agent-shunt` on File-Hit@5 and MRR while using much less source context.

Hono excludes `bun.lock` in the corpus globs for **both systems**. An earlier measurement allowed the large lockfile into `rg`'s top results and materially inflated its context baseline. That run was discarded and all published Hono numbers above come from the corrected rerun.

### Aggregate top-N trade-off

| Files opened by `rg` | `rg` File-Hit@K | `agent-shunt` File-Hit@K | `rg` whole-file context | agent context | Reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| top 1 | 28% | **50%** | 658,290 | 51,835 | **92.13%** |
| top 3 | 62% | **76%** | 1,590,547 | 51,835 | **96.74%** |
| top 5 | 74% | **78%** | 2,432,356 | 51,835 | **97.87%** |

This table exposes the actual trade-off rather than treating context reduction alone as success. Opening more `rg` files improves file discovery, but increases source context substantially.

## Token measurement

These are **estimated context tokens**, not provider billing tokens. All context comparisons use the same conservative estimator used by the production retrieval budget, including the same numbered source representation and per-item envelope.

The benchmark independently re-estimates returned `agent-shunt` chunks and fails if the value differs from production `estimatedTokens`. Provider-specific tokenizers can produce different absolute counts, so reduction and compression ratios are the more robust comparison metrics.

## Quality measurement

- **Chunk Hit@K** is the fraction of questions where at least one acceptable human-authored ground-truth target appears within the first K returned chunks.
- **File Hit@K** is the equivalent metric over distinct ranked files.
- **MRR** is the mean reciprocal rank of the first acceptable target in the corresponding ranking.
- These metrics measure retrieval quality, not whether an LLM would produce a correct final answer.
- Each repository contributes exactly 10 questions, so the five-repository aggregate is the 50-case aggregate.

The savings figures should always be published together with quality. A system that returned almost no context would have excellent compression but poor retrieval usefulness.

## Pinned corpora

All corpora contain 10 human-authored questions and use a 1,200-token retrieval budget, 8 context lines, and pinned immutable Git commits.

| Corpus | Repository | Full commit |
| --- | --- | --- |
| `ripgrep-v1` | https://github.com/BurntSushi/ripgrep | `3fce3b5bb0236da2df6d99672afb8a719642eca7` |
| `flask-v1` | https://github.com/pallets/flask | `d73fa1cdcbd8b1465c151db8924ba58b1dd14e35` |
| `hono-v1` | https://github.com/honojs/hono | `098e11912ab244c5c33931de007f04dc8e3c2929` |
| `cobra-v1` | https://github.com/spf13/cobra | `adbc8813901bba65827259daa8e22ff94ec1f30e` |
| `gson-v1` | https://github.com/google/gson | `854c8255b625cf1e13c701a83ea9ccb4caaa576a` |

## Reproduce

Clone one of the repositories at its pinned commit. For the conservative same-file comparison:

```bash
cargo run --release --example context_savings_eval -- \
  eval/external/<corpus>.json \
  --root /path/to/pinned/repository
```

For the `rg + whole-file reads` comparison:

```bash
cargo run --release --example rg_baseline_eval -- \
  eval/external/<corpus>.json \
  --root /path/to/pinned/repository
```

The external evaluation and ripgrep-baseline GitHub Actions workflows run these measurements against all five pinned repositories.

## Publication-safe wording

Two concise claims supported by the current benchmark are:

> In a reproducible 50-question benchmark across five pinned open-source repositories and five languages, agent-shunt reduced estimated source context by **94.28%** compared with reading the exact same selected files in full, while lexical retrieval reached **78% Chunk-Hit@5** under a 1,200-token budget.

> Against a disclosed plain-ripgrep baseline that searches the same preprocessed query terms and reads its top five matching files in full, agent-shunt used **97.87% less estimated source context** while achieving **78% vs 74% File-Hit@5** and **0.611 vs 0.489 file MRR** across the same 50 questions.

> Against budgeted plain-ripgrep targeted navigation using the same preprocessed query terms and the same 1,200-token ceiling, agent-shunt reached **78% Chunk-Hit@5** versus **34% for fixed-window reads** and **36% for enclosing-symbol reads**, while using less estimated context on the same 50-question corpus.

Do not rewrite these as "94%/98% lower LLM cost", "fewer billed tokens", or "better than coding agents". Those claims were not measured by these benchmarks.
