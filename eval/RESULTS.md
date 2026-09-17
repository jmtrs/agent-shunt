# Context savings benchmark results

Date: 2026-09-17

These results measure how much source context `agent-shunt` lexical retrieval avoids sending after it has already identified useful files.

## Headline

Across **50 architecture and implementation questions** over five pinned open-source repositories in Rust, Python, TypeScript, Go, and Java, `agent-shunt` delivered **51,864 estimated context tokens** versus **912,071** tokens for reading the exact same selected files in full.

That is an aggregate **94.3% context reduction**, or **17.59x less context**, with aggregate lexical retrieval quality of **Hit@1 50%**, **Hit@3 74%**, **Hit@5 78%**, and **MRR 0.602**.

The comparison is deliberately conservative. The baseline is given perfect file selection for free: for each question it reads only the distinct files that actually contributed chunks selected by `agent-shunt`. It does not read the whole repository and it does not include extra files returned by a broader search.

## Results by repository

All corpora contain 10 human-authored questions and use a 1,200-token retrieval budget, 8 context lines, and pinned immutable Git commits.

| Repository | Language | Commit | Hit@1 | Hit@3 | Hit@5 | MRR | Delivered tokens | Same files in full | Reduction | Compression | Median case reduction | p95 delivered | p95 full files |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ripgrep | Rust | `3fce3b5bb023` | 40% | 80% | 90% | 0.587 | 10,066 | 406,114 | **97.5%** | **40.35x** | 97.0% | 1,200 | 117,616 |
| Flask | Python | `d73fa1cdcbd8` | 60% | 70% | 70% | 0.633 | 11,415 | 149,620 | **92.4%** | **13.11x** | 89.1% | 1,189 | 36,171 |
| Hono | TypeScript | `098e11912ab2` | 30% | 60% | 60% | 0.417 | 10,002 | 92,994 | **89.2%** | **9.30x** | 85.8% | 1,194 | 31,702 |
| Cobra | Go | `adbc8813901b` | 90% | 90% | 100% | 0.925 | 11,039 | 124,082 | **91.1%** | **11.24x** | 92.1% | 1,191 | 27,427 |
| Gson | Java | `854c8255b625` | 30% | 70% | 70% | 0.450 | 9,342 | 139,261 | **93.3%** | **14.91x** | 92.4% | 1,189 | 36,941 |
| **Aggregate, 50 cases** | 5 languages | pinned above | **50%** | **74%** | **78%** | **0.602** | **51,864** | **912,071** | **94.3%** | **17.59x** | n/a | n/a | n/a |

Average context per question was **1,037 estimated tokens** from `agent-shunt` versus **18,241 estimated tokens** for reading the same selected files in full.

## Baseline definition

For every question:

1. Run the production lexical retrieval path with the corpus settings.
2. Record the chunks that survive ranking, chunk selection, MMR, relevance filtering, and the 1,200-token budget.
3. Collect the distinct source files represented by those chunks.
4. Reload only those exact files through `SecureFilesystem`.
5. Compare the token estimate for the selected chunks with the token estimate for those same files in full.

This baseline intentionally does **not** measure against the entire repository. It answers a narrower and harder question:

> If another system somehow knew exactly which files `agent-shunt` selected, how much context would focused chunk retrieval still save compared with reading those files completely?

## Token measurement

These are **estimated context tokens**, not provider billing tokens. Both sides use the same conservative estimator used by the production retrieval budget, including the same numbered source representation and per-item envelope. The benchmark independently re-estimates returned chunks and fails if the value differs from production `estimatedTokens`.

Because the same estimator is used for both sides, the reduction and compression ratios are the primary comparison metrics. Provider-specific tokenizers can produce different absolute token counts.

## Quality measurement

- **Hit@K** is the fraction of questions where at least one acceptable human-authored ground-truth target appears within the first K returned chunks.
- **MRR** is the mean reciprocal rank of the first acceptable target.
- These metrics measure retrieval quality, not whether an LLM would produce a correct final answer.
- Each repository contributes the same number of questions, so aggregate Hit@K and MRR are the simple 50-case aggregate.

The savings result should always be published together with quality. A system that returned almost no context would have excellent compression but poor retrieval usefulness.

## Pinned corpora

| Corpus | Repository | Full commit |
| --- | --- | --- |
| `ripgrep-v1` | https://github.com/BurntSushi/ripgrep | `3fce3b5bb0236da2df6d99672afb8a719642eca7` |
| `flask-v1` | https://github.com/pallets/flask | `d73fa1cdcbd8b1465c151db8924ba58b1dd14e35` |
| `hono-v1` | https://github.com/honojs/hono | `098e11912ab244c5c33931de007f04dc8e3c2929` |
| `cobra-v1` | https://github.com/spf13/cobra | `adbc8813901bba65827259daa8e22ff94ec1f30e` |
| `gson-v1` | https://github.com/google/gson | `854c8255b625cf1e13c701a83ea9ccb4caaa576a` |

## Reproduce

Clone one of the repositories at its pinned commit, then run:

```bash
cargo run --release --example context_savings_eval -- \
  eval/external/<corpus>.json \
  --root /path/to/pinned/repository
```

The external evaluation workflow runs this comparison for all five pinned repositories.

## Publication-safe wording

A concise claim supported by this benchmark is:

> In a reproducible 50-question benchmark across five pinned open-source repositories and five languages, agent-shunt reduced estimated source context by 94.3% compared with reading the exact same selected files in full, while lexical retrieval reached 78% Hit@5 under a 1,200-token budget.

Do not rewrite this as "94.3% lower LLM cost", "94.3% fewer billed tokens", or "94.3% better than coding agents". Those claims were not measured by this benchmark.
