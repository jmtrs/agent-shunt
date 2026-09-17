# Retrieval evaluation

This directory contains the deterministic retrieval quality gate for `agent-shunt`.
It does not call an LLM or an embeddings provider. The first corpus compares the
local lexical pipeline with lexical retrieval plus pseudo-relevance feedback
(`--prf`).

Run it from the repository root:

```bash
cargo run --release --example retrieval_eval -- eval/corpus.json
```

Machine-readable output:

```bash
cargo run --release --example retrieval_eval -- eval/corpus.json --json
```

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

## What this corpus does not prove

`agent-shunt-self-v1` asks questions about this repository itself. It protects
known retrieval behaviour, but it is not an independent benchmark. The next
step is to add version-pinned external repositories and human-authored ground
truth across Rust, TypeScript, Python, Go, and Java. Those results are the ones
that should support README claims about retrieval quality or token savings.
