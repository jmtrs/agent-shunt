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

To make this baseline harder to beat, targeted reads are **interleaved by ranked file**: the best hit from each ranked file is considered before taking a second hit from any file.

| Repository | agent Hit@5 | agent MRR | agent tokens | `rg + window` Hit@5 | window MRR | window tokens | `rg + symbol` Hit@5 | symbol MRR | symbol tokens |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | **90%** | **0.587** | **10,066** | 40% | 0.220 | 11,761 | 40% | 0.220 | 11,767 |
| Flask | **70%** | **0.633** | **11,415** | 60% | 0.400 | 11,683 | **70%** | 0.420 | 11,673 |
| Hono | **90%** | **0.500** | **9,583** | **90%** | **0.500** | 11,737 | **90%** | **0.500** | 11,703 |
| Cobra | **100%** | **0.925** | **11,039** | **100%** | 0.717 | 11,604 | **100%** | 0.717 | 11,675 |
| Gson | **70%** | **0.450** | **9,342** | **70%** | 0.392 | 11,587 | 60% | 0.367 | 11,772 |
| **Aggregate, 50 cases** | **84%** | **0.619** | **51,445** | 72% | 0.446 | 58,372 | 72% | 0.445 | 58,590 |

At the same nominal context ceiling:

| Strategy | Hit@1 | Hit@3 | Hit@5 | MRR | Avg. context/query |
| --- | ---: | ---: | ---: | ---: | ---: |
| `agent-shunt` lexical | **50%** | **76%** | **84%** | **0.619** | **1,028.9** |
| `rg + window` | 26% | 64% | 72% | 0.446 | 1,167.4 |
| `rg + symbol` | 26% | 64% | 72% | 0.445 | 1,171.8 |

The stronger baseline narrows the comparison without relying on wasteful whole-file reads. Aggregate Hit@5 is **84% vs 72%**, MRR is **0.619 vs 0.446/0.445**, and agent-shunt delivers about 12% less estimated context. Hono is now a tie at 90% Hit@5 / 0.500 MRR, rather than a targeted-`rg` win.

This still does **not** prove superiority to a coding agent. A real agent may reformulate queries, follow references, use an LSP, inspect repository maps, or perform multiple adaptive search/read rounds.

The machine-readable snapshot is [`eval/results/2026-09-18-targeted-rg.json`](results/2026-09-18-targeted-rg.json), validated by workflow run `35335856826`.

## Semantic retrieval quality

The opt-in local semantic path uses the production confidence-gated fusion policy with `bge-small-en-v1.5` and dense top-k 24. It was evaluated on the same 50 questions and 1,200-token budget.

| Repository | Lexical Hit@1 | Hit@3 | Hit@5 | Lexical MRR | Semantic Hit@1 | Hit@3 | Hit@5 | Semantic MRR |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | 40% | **80%** | 90% | 0.587 | 40% | 70% | 90% | **0.595** |
| Flask | 60% | 70% | 70% | 0.633 | 60% | **90%** | **90%** | **0.750** |
| Hono | 30% | **70%** | **90%** | **0.500** | 30% | 70% | 80% | 0.475 |
| Cobra | 90% | 90% | 100% | 0.925 | 90% | **100%** | 100% | **0.950** |
| Gson | 30% | 70% | 70% | 0.450 | 30% | **80%** | **80%** | **0.517** |
| **Aggregate, 50 cases** | **50%** | 76% | 84% | 0.619 | **50%** | **82%** | **88%** | **0.657** |

Aggregate Hit@1 is unchanged. Semantic retrieval adds **6 percentage points at Hit@3**, **4 at Hit@5**, and improves MRR by about **0.038**. Average delivered context is **1,050.6 estimated tokens/query** versus **1,028.9** for lexical retrieval.

The result is not a universal win. On ripgrep, semantic Hit@3 falls from 80% to 70% while Hit@5 stays 90%. On Hono, the improved lexical ranking now outperforms semantic fusion: **90% vs 80% Hit@5** and **0.500 vs 0.475 MRR**. Those regressions are kept visible and motivate a separate semantic-fusion change rather than weakening the lexical fix.

The production policy remains bounded: lexical #1 stays authoritative, at most one semantic region is admitted, the leading semantic file must clear a confidence margin, and all evidence competes under the same token budget.

The semantic workflow now runs on retrieval/ranking PRs, caches per-corpus vectors, and validates all five pinned corpora. Final validation: workflow run `35336476560`.

The machine-readable summary is [`eval/results/2026-09-18-semantic.json`](results/2026-09-18-semantic.json).

## 1. Same selected files in full

This comparison gives the baseline perfect file selection for free: it reads the distinct files that actually contributed chunks selected by `agent-shunt`.

| Repository | Language | Commit | Chunk Hit@1 | Hit@3 | Hit@5 | Chunk MRR | Delivered tokens | Same files in full | Reduction | Compression | Median case reduction | p95 delivered | p95 full files |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | Rust | `3fce3b5bb023` | 40% | 80% | 90% | 0.587 | 10,066 | 406,114 | **97.5%** | **40.35x** | 97.0% | 1,200 | 117,616 |
| Flask | Python | `d73fa1cdcbd8` | 60% | 70% | 70% | 0.633 | 11,415 | 149,620 | **92.4%** | **13.11x** | 89.1% | 1,189 | 36,171 |
| Hono | TypeScript | `098e11912ab2` | 30% | 70% | 90% | 0.500 | 9,583 | 102,486 | **90.6%** | **10.69x** | 88.4% | 1,195 | 31,702 |
| Cobra | Go | `adbc8813901b` | 90% | 90% | 100% | 0.925 | 11,039 | 123,636 | **91.1%** | **11.20x** | 81.6% | 1,191 | 32,260 |
| Gson | Java | `854c8255b625` | 30% | 70% | 70% | 0.450 | 9,342 | 139,261 | **93.3%** | **14.91x** | 92.4% | 1,189 | 36,941 |
| **Aggregate, 50 cases** | 5 languages | pinned below | **50%** | **76%** | **84%** | **0.619** | **51,445** | **921,117** | **94.41%** | **17.90x** | n/a | n/a | n/a |

Average context per question is **1,029 estimated tokens** from `agent-shunt` versus **18,422 estimated tokens** for reading those same selected files in full.

## 2. Plain ripgrep + whole-file reads

This baseline models a simple search-first workflow. Plain `rg` receives the same preprocessed query terms as agent-shunt, ranks files by distinct term coverage and matching-line count, then reads the top files in full.

| Repository | agent File-Hit@5 | `rg` File-Hit@5 | agent File MRR | `rg` File MRR | agent context | `rg` top-5 full-file context | Context reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | **90%** | 40% | **0.587** | 0.289 | 10,066 | 936,411 | **98.93%** |
| Flask | 70% | 70% | **0.650** | 0.448 | 11,415 | 592,814 | **98.07%** |
| Hono | **90%** | **90%** | 0.500 | **0.512** | 9,583 | 220,447 | **95.65%** |
| Cobra | **100%** | **100%** | **0.950** | 0.717 | 11,039 | 381,187 | **97.10%** |
| Gson | 70% | **80%** | **0.450** | 0.427 | 9,342 | 334,283 | **97.21%** |
| **Aggregate, 50 cases** | **84%** | 76% | **0.627** | 0.479 | **51,445** | **2,465,142** | **97.91%** |

### Aggregate top-N trade-off

| Files opened by `rg` | `rg` File-Hit@K | `agent-shunt` File-Hit@K | `rg` whole-file context | agent context | Reduction |
| --- | ---: | ---: | ---: | ---: | ---: |
| top 1 | 26% | **50%** | 634,107 | 51,445 | **91.89%** |
| top 3 | 64% | **78%** | 1,608,472 | 51,445 | **96.80%** |
| top 5 | 76% | **84%** | 2,465,142 | 51,445 | **97.91%** |

Plain `rg` still wins Gson File-Hit@5 and narrowly edges Hono file MRR. Those cases remain visible.

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

Three concise claims supported by the current benchmark are:

> In a reproducible 50-question benchmark across five pinned open-source repositories and five languages, agent-shunt reduced estimated source context by **94.41%** compared with reading the exact same selected files in full, while lexical retrieval reached **84% Chunk-Hit@5** under a 1,200-token budget.

> Against a disclosed plain-ripgrep baseline that searches the same preprocessed query terms and reads its top five matching files in full, agent-shunt used **97.91% less estimated source context** while achieving **84% vs 76% File-Hit@5** and **0.627 vs 0.479 file MRR** across the same 50 questions.

> Against budgeted plain-ripgrep targeted navigation using the same preprocessed query terms and 1,200-token ceiling, agent-shunt reached **84% Chunk-Hit@5** versus **72%** for both targeted baselines, with **0.619 MRR vs 0.446/0.445** and lower estimated context.

Do not rewrite these as lower provider cost, fewer billed tokens, or superiority to coding agents. Those claims were not measured.

