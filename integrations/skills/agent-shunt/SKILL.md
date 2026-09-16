---
name: agent-shunt
description: Retrieve and analyze bounded code context with the local agent-shunt CLI. Use for broad code-location questions, module summaries, call tracing, related-test discovery, log analysis, or when a whole file would otherwise be read. Prefer targeted native reads for known symbols or explicit line ranges.
---

# Agent Shunt

Use deterministic retrieval first:

```bash
agent-shunt retrieve --question "<current task>" --dir "<repository root>"
```

Inspect the returned line-numbered chunks. Treat ranking and prose as hints; source ranges are evidence.

Use the worker (costs money — remote API) only when the selected context still needs synthesis:

```bash
agent-shunt retrieve --analyze --question "<current task>" --dir "<repository root>"
```

Run deterministic `retrieve` (free) first; escalate to `--analyze` only if the chunks alone do not answer the question. Do not re-run `--analyze` on the same question.

## Scope retrieval to changed files

For diff/PR review, restrict retrieval to locally changed files (tracked modifications + untracked). Bare `--diff` uses HEAD; pass a base ref to compare against it:

```bash
agent-shunt retrieve --diff --question "<what changed>" --dir "<repo>"
agent-shunt retrieve --diff origin/develop --question "<what changed>" --dir "<repo>"
```

## Keep unsafe or noisy paths out

The warning below has flags to enforce it — use them. Exclude secrets, generated artifacts, databases, binaries, vendored trees:

```bash
agent-shunt retrieve --question "..." --glob 'src/**' --exclude 'dist/**' --exclude 'public/**'
```

`--glob` accepts `!`-prefixed excludes and is repeatable; `--exclude <pat>` is sugar for `--glob '!<pat>'`.

Use `scan --path ...` only for explicitly chosen files. Preview what a scan would send before spending, especially for dubious paths:

```bash
agent-shunt scan --path <file> --question "..." --dry-run
```

Do not pass secrets, generated artifacts, databases, binaries, or unrelated files.

## Tuning

`--budget-tokens` (default 12000), `--context-lines` (default 8), `--max-hits` (default 200), `--model <id>` override the retrieval envelope when chunks are truncated or the repo is large. `--mmr-lambda`, `--max-block-lines`, `--min-score-percent` tune diversity, chunk size, and the relevance floor.

Two further opt-in escalations cost money like `--analyze` (they send chunks to a provider) — use only when free lexical retrieval misses code that matches by meaning rather than by terms:

```bash
agent-shunt retrieve --semantic --question "..." --dir "<repo>"   # dense whole-repo recall (needs embeddingModel in config)
agent-shunt retrieve --rerank   --question "..." --dir "<repo>"   # worker model reorders the top chunks
```

Prefer plain `retrieve` first; reach for `--semantic`/`--rerank` only when it comes back thin.

## Output contract — verify, do not trust

Retrieval returns `chunks[]` (path + startLine/endLine = evidence). `--analyze` adds `summary`, `verifyHint` (a `sed -n` command per chunk), `uncertainties[]`, and `trust: untrusted-model-output-with-validated-source-references`. The `trust` field is literal: only the file and line references are validated. Confirm any claim against the source range (run the `verifyHint`) before relying on it. Never present worker prose as verified fact.

## Fallbacks and health

Use normal `rg` and ranged reads when the exact file, symbol, or line range is already known. Never replace a small targeted read with the shunt.

If the CLI seems unavailable or misconfigured, check locally (no remote request) before falling back:

```bash
agent-shunt doctor   # healthy, ripgrep availability, credential discovery
agent-shunt check    # config + credential source
```

If retrieval finds no useful evidence, every worker model fails, or the CLI is unavailable, continue with normal targeted repository reads. Do not block the task and do not claim that model output is verified beyond its locally validated file and line references.
