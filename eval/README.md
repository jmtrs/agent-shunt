# Retrieval evaluation

This directory contains the deterministic retrieval quality gate for `agent-shunt`.
It does not call an LLM or an embeddings provider. The first corpus compares the
local lexical pipeline with lexical retrieval plus pseudo-relevance feedback
(`--prf`).

Run the self-retrieval corpus from the repository root:

```bash
cargo run --release --example retrieval_eval -- eval/corpus.json
```

Machine-readable output:

```bash
cargo run --release --example retrieval_eval -- eval/corpus.json --json
```

## External repositories

The evaluator can also run against a different checkout without copying the
benchmark harness into that repository:

```bash
cargo run --release --example retrieval_eval -- \
  eval/external/example.json \
  --root /path/to/external/repository
```

An external corpus should pin the exact Git tree it was authored against:

```json
{
  "version": 1,
  "name": "example-repo-v1",
  "repository": {
    "url": "https://github.com/example/project",
    "commit": "0123456789abcdef0123456789abcdef01234567"
  }
}
```

`commit` must be a full 40-character Git SHA. Before retrieval starts, the
evaluator checks that `git rev-parse HEAD` equals that SHA and that the checkout
is completely clean, including untracked files. A mismatch fails the run. This
keeps source ranges and retrieval results tied to one immutable tree rather
than whatever happens to be checked out locally.

The repository URL is provenance metadata. Reproducibility is enforced by the
commit and clean working tree.

## Metrics

- **Hit@K**: fraction of questions with at least one acceptable ground-truth
  target in the first K returned chunks. Each entry in a case's `expected`
  array is an acceptable alternative, so this is a per-question success metric,
  not recall over a set of required evidence items.
- **MRR**: mean reciprocal rank of the first matching ground-truth chunk.
- **avg tokens**: mean `estimatedTokens` delivered by retrieval. This is bounded
  by the corpus token budget.
- **avg/p95 ms**: wall-clock retrieval latency for the cases in that run. Latency
  is reported for visibility but is not gated yet because shared CI runners are
  noisy.

A case can name more than one acceptable evidence target. A target can match a
whole file:

```json
{ "path": "src/application/retrieve.rs" }
```

or a specific source range:

```json
{
  "path": "src/application/retrieve.rs",
  "startLine": 120,
  "endLine": 180
}
```

A retrieved chunk satisfies a ranged target when their line spans overlap.

## Context savings benchmark

`context_savings_eval` measures how much context the lexical pipeline avoids
sending after it has already found the relevant files:

```bash
cargo run --release --example context_savings_eval -- \
  eval/external/example.json \
  --root /path/to/external/repository
```

Use `--json` for machine-readable output.

The primary baseline is intentionally conservative: for each question, take the
**exact distinct files that contributed the chunks selected by `agent-shunt`**
and compare the delivered chunks with sending those same files in full. It does
not compare against the whole repository and it does not add files that retrieval
never selected. This isolates the context reduction produced by focused chunking
once file discovery has already succeeded.

Both sides use the same conservative token estimator and the same numbered source
format. The benchmark independently re-estimates the selected chunks and fails
if that value differs from production `estimatedTokens`, preventing silent metric
drift. The whole-file side reloads the selected files through `SecureFilesystem`,
so it uses the same UTF-8, binary, size, path-safety and line-normalization rules
as production retrieval.

Reported savings metrics are:

- **context reduction %**: `1 - delivered tokens / whole-selected-file tokens`,
  computed from aggregate token totals. This is the primary publishable figure.
- **compression ratio**: whole-selected-file tokens divided by delivered tokens.
- **median case reduction %**: median reduction across individual questions, so
  a few unusually large source files cannot hide typical-case behaviour.
- **p95 delivered / whole-file tokens**: tail context size for both sides.

These are estimator-based context measurements, not provider billing-token
claims and not measurements of a proprietary coding agent. Public results should
always name the corpus version, pinned repository commits, token budget, quality
metrics and baseline definition alongside the savings number.


## Ripgrep navigation baselines

`rg_baseline_eval` also measures two stronger search-first baselines under the
**same token budget** as agent-shunt:

- **rg + fixed window**: rank plain-ripgrep matches, then read the configured
  context window around each hit until the budget is exhausted.
- **rg + enclosing symbol**: use the same plain-ripgrep ranking, then expand each
  hit to its enclosing function/method/class when a bounded structural span is
  available, falling back to the fixed window otherwise.

Both variants deduplicate overlapping reads and stop at the corpus token budget.
Hits are interleaved by ranked file: the best hit from each ranked file is
considered before a second hit from any file, so the baseline cannot spend most
of its budget drilling into one leader. They deliberately do not use
agent-shunt's filename/path boosts, IDF weighting, MMR, PRF, semantic retrieval,
or relevance pruning. The symbol variant shares only the structural resolver so
the comparison isolates retrieval/ranking rather than penalizing the baseline
for reading an arbitrary line window.

This is a harder comparison than `rg + whole-file reads`: it asks whether
agent-shunt retrieves better evidence than a competent targeted-navigation
workflow at roughly the same context allowance, not merely whether chunking
saves tokens versus opening entire files.

## Semantic retrieval benchmark

`semantic_eval` compares the production lexical path with the opt-in local
semantic path on the same pinned corpus, question set, token budget, chunking,
MMR, and ground truth:

```bash
cargo run --release --features local-embed --example semantic_eval -- \
  eval/external/example.json \
  --root /path/to/external/repository
```

Use `--json` for machine-readable output. The evaluator uses
`bge-small-en-v1.5` with dense top-k 24 and reports the one-time cold index
build separately from warmed per-query latency. Semantic results therefore
measure the actual production fusion policy rather than a standalone vector
search.

The GitHub Actions `Semantic retrieval evaluation` workflow runs the same
comparison across all five pinned external corpora. It runs at most two corpora
in parallel and reuses both the model cache and per-corpus vector cache.
Latency is informational only; Hit@K, MRR, and delivered context are the
portable quality measurements.

## Gates

`gates` makes quality regressions executable instead of descriptive. Each
strategy may require minimum Hit@K / MRR and a maximum average token count:

```json
{
  "gates": {
    "lexical": {
      "hitAt": { "5": 0.75 },
      "minMrr": 0.45,
      "maxAvgTokens": 1200
    }
  }
}
```

The evaluator exits non-zero when a gate fails, so it can run directly in CI.
For compatibility with existing version-1 corpus files, the harness still
accepts `recallAt` as an input alias, but reports and new corpora use `hitAt`.
The initial thresholds are intentionally conservative: this self-retrieval
corpus is a regression seed, not evidence of general retrieval quality.

## What the self corpus does not prove

`agent-shunt-self-v1` asks questions about this repository itself. It protects
known retrieval behaviour, but it is not an independent benchmark. External
corpora should use version-pinned repositories and human-authored ground truth
across Rust, TypeScript, Python, Go, and Java. Those results are the ones that
should support README claims about retrieval quality or token savings.