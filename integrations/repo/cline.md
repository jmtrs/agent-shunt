# Agent Shunt

Use the local `agent-shunt` CLI for broad code questions (locating features, module summaries, call tracing, related-test discovery) instead of reading whole files.

Deterministic retrieval first:

```bash
agent-shunt retrieve --question "<current task>" --dir "<repository root>"
```

Inspect the returned line-numbered chunks. Treat ranking and prose as hints; source ranges are evidence.

Comes back thin? Escalate cheapest-first (each calls a provider): `--expand` adds related search terms, `--semantic` recalls the whole repo by embeddings, `--rerank` reorders the top chunks, `--analyze` synthesizes an answer with locally validated citations:

```bash
agent-shunt retrieve --analyze --question "<current task>" --dir "<repository root>"
```

Use `scan --path ...` only for explicitly chosen files. Do not pass secrets, generated artifacts, databases, binaries, or unrelated files.

Reviewing a change or freshly written code? Add `--diff` to scope retrieval to what you touched — tracked modifications and untracked new files — which plain retrieval can otherwise bury under older files.

Use normal search and ranged reads when the exact file, symbol, or line range is already known. Never replace a small targeted read with the shunt.

If retrieval finds no useful evidence, every worker model fails, or the CLI is unavailable, continue with normal targeted repository reads. Do not block the task and do not claim that model output is verified beyond its locally validated file and line references.
