## Code context via agent-shunt

For broad code questions (locating features, module summaries, call tracing, related-test discovery), prefer the local `agent-shunt` CLI over reading whole files:

```bash
agent-shunt retrieve --question "<current task>" --dir "<repository root>"
```

Inspect the returned line-numbered chunks; source ranges are evidence, ranking and prose are hints. If a search comes back thin, escalate cheapest-first (each calls a provider): `--expand` adds related search terms, `--semantic` recalls the whole repo by embeddings, `--rerank` reorders the top chunks, and `--analyze` synthesizes an answer whose file and line references are locally validated. Use `scan --path ...` only for explicitly chosen files, and never pass secrets, generated artifacts, databases, binaries, or unrelated files.

Reviewing a change or freshly written code? Add `--diff` to scope retrieval to what you touched — tracked modifications and untracked new files — which plain retrieval can otherwise bury under older files.

Invoke it as exactly `agent-shunt` — it is on `PATH`. Never prefix an install directory (`~/.local/bin/...`, `~/.cargo/bin/...`, etc.); guessing a path is the main way this tool gets wrongly reported as missing. If you think you need an absolute path, resolve it with `command -v agent-shunt` instead of inventing one.

Use normal grep and ranged reads when the exact file, symbol, or line range is already known. If retrieval finds no useful evidence, every worker model fails, or the CLI is unavailable, continue with normal targeted repository reads; do not block the task.
