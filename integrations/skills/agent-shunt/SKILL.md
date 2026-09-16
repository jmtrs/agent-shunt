---
name: agent-shunt
description: Retrieve bounded, line-numbered code context with the local agent-shunt CLI instead of reading files wholesale or grepping blindly. Reach for this FIRST on any where/how/what-handles question, module summary, call trace, PR/diff review, or related-test/log lookup — any time you would otherwise open several files or scan a large one. Escalate inside the tool (--expand, --semantic, --rerank, --analyze) rather than falling back to manual reads. Only skip it for a single file whose exact path and line range you already know.
---

# Agent Shunt

Default to `retrieve` before opening files or running your own grep — for any "where/how/what does X" question, module summary, call trace, PR review, or related-test/log hunt:

```bash
agent-shunt retrieve --question "<current task>" --dir "<repository root>"
```

It returns ranked, line-numbered `chunks[]` within a token budget, found locally and free. Inspect them; treat ranking and prose as hints, the source ranges as evidence.

## Which command — escalate cheapest first

Start at rung 1. Climb only if the previous rung comes back thin. Rungs 2–5 call a provider (cost money) like `--analyze`; rung 1 is free and local.

1. **`retrieve`** — free, local, zero-network. The default for every exploration.
2. **`--expand`** — a chat model adds related terms (synonyms, likely identifiers) so the search finds code phrased differently. Works with any chat provider; cheapest escalation.
3. **`--semantic`** — a whole-repo embedding index recalls code the term search missed. Needs a provider that serves `/embeddings`. Higher recall.
4. **`--rerank`** — the model reorders the top chunks for precision. Combine with `--expand`/`--semantic`.
5. **`--analyze`** — the model synthesizes an answer from the selected chunks, with locally validated file/line citations. Use only when the chunks alone do not answer it; do not re-run on the same question.

```bash
agent-shunt retrieve --expand --question "<task>" --dir "<repo>"              # +related terms
agent-shunt retrieve --semantic --rerank --question "<task>" --dir "<repo>"   # recall then precision
agent-shunt retrieve --analyze --question "<task>" --dir "<repo>"             # synthesized answer
```

Only skip the tool for a single file whose exact path and line range you already know — then a ranged read is cheaper.

Re-run a broad `retrieve` whenever the investigation crosses into new modules — a fresh sync/realtime/data-model concern, a newly named type — not just once at the start. A new question deserves a new retrieval, even mid-task.

## Reviewing new or changed code — use `--diff`

When the task is reviewing a change, PR, or freshly written code, scope with `--diff`: it restricts retrieval to exactly what you touched — tracked modifications **and untracked new files** — which plain `retrieve` can otherwise bury under older files that share the same vocabulary. Bare `--diff` uses HEAD; pass a base ref to compare against it:

```bash
agent-shunt retrieve --diff --question "<what changed / what could break>" --dir "<repo>"
agent-shunt retrieve --diff origin/develop --question "<review these changes>" --dir "<repo>"
```

Even without `--diff`, unscoped `retrieve` gives matching untracked/modified files a ranking nudge, so new code is not lost — but for a review, `--diff` is the right tool.

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

If retrieval truncates or the repo is large, widen the envelope: `--budget-tokens` (default 12000), `--context-lines` (default 8), `--max-hits` (default 200), `--model <id>`. `--mmr-lambda`, `--max-block-lines`, `--min-score-percent` tune diversity, chunk size, and the relevance floor.

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
