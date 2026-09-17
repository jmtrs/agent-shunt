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

- **Recall@K**: fraction of questions whose first matching ground-truth evidence
  appears in the first K returned chunks.
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

## Gates

`gates` makes quality regressions executable instead of descriptive. Each
strategy may require minimum Recall@K / MRR and a maximum average token count:

```json
{
  "gates": {
    "lexical": {
      "recallAt": { "5": 0.75 },
      "minMrr": 0.45,
      "maxAvgTokens": 1200
    }
  }
}
```

The evaluator exits non-zero when a gate fails, so it can run directly in CI.
The initial thresholds are intentionally conservative: this self-retrieval
corpus is a regression seed, not evidence of general retrieval quality.

## What the self corpus does not prove

`agent-shunt-self-v1` asks questions about this repository itself. It protects
known retrieval behaviour, but it is not an independent benchmark. External
corpora should use version-pinned repositories and human-authored ground truth
across Rust, TypeScript, Python, Go, and Java. Those results are the ones that
should support README claims about retrieval quality or token savings.
