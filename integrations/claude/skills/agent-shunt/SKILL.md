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

Use the low-cost worker only when the selected context still needs synthesis:

```bash
agent-shunt retrieve --analyze --question "<current task>" --dir "<repository root>"
```

Use `scan --path ...` only for explicitly chosen files. Do not pass secrets, generated artifacts, databases, binaries, or unrelated files.

Use normal `rg` and ranged reads when the exact file, symbol, or line range is already known. Never replace a small targeted read with the shunt.

If retrieval finds no useful evidence, every worker model fails, or the CLI is unavailable, continue with normal targeted repository reads. Do not block the task and do not claim that model output is verified beyond its locally validated file and line references.
