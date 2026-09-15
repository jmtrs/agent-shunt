## Code context via agent-shunt

For broad code questions (locating features, module summaries, call tracing, related-test discovery), prefer the local `agent-shunt` CLI over reading whole files:

```bash
agent-shunt retrieve --question "<current task>" --dir "<repository root>"
```

Inspect the returned line-numbered chunks; source ranges are evidence, ranking and prose are hints. When the selected context still needs synthesis, add `--analyze` for a low-cost worker summary whose file and line references are locally validated. Use `scan --path ...` only for explicitly chosen files, and never pass secrets, generated artifacts, databases, binaries, or unrelated files.

Use normal grep and ranged reads when the exact file, symbol, or line range is already known. If retrieval finds no useful evidence, every worker model fails, or the CLI is unavailable, continue with normal targeted repository reads; do not block the task.
