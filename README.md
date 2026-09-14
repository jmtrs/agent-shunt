# agent-shunt

Provider-neutral, read-only bulk context worker for coding agents. It sends explicitly selected text files to a low-cost OpenRouter model and validates every returned file and line reference locally.

## Requirements

- Node.js 20+
- `OPENROUTER_API_KEY`, or an existing key in one of:
  - `~/.config/agent-shunt/.env`
  - `~/.config/claude-openrouter/.env`
  - `~/.config/or-info/.env`

Keys are loaded at runtime and never copied into this repository.

## Usage

```bash
npm link
agent-shunt check
agent-shunt scan \
  --question "Where is authentication enforced?" \
  --path src/auth.js \
  --path src/routes.js
```

`check` confirms local configuration and credential discovery only. A real `scan` is required to validate the credential with OpenRouter.

Validate inputs without sending anything:

```bash
agent-shunt scan --dry-run --question "Summarize this file" --path src/app.js
```

## Global configuration

Optional `~/.config/agent-shunt/config.json`:

```json
{
  "model": "qwen/qwen3.7-flash",
  "timeoutMs": 60000,
  "maxOutputTokens": 2000,
  "maxResponseBytes": 1000000,
  "maxQuestionBytes": 16000,
  "maxRequestBytes": 4000000,
  "maxFiles": 30,
  "maxFileBytes": 512000,
  "maxTotalBytes": 2000000
}
```

CLI options override global configuration. Paths must resolve inside `--cwd` (the current directory by default); symlink escapes, directories, oversized inputs, and binary files are rejected.

Every request requires structured-output support, denies data-collection providers, and enforces Zero Data Retention routing. Models ending in `:free` and the `openrouter/free` router are blocked by default.

The API origin is fixed to `https://openrouter.ai/api/v1` so a configuration mistake cannot redirect the Bearer token. Returned prose remains untrusted model output; only source paths and line ranges are locally validated.

## Status

The initial version exposes the validated CLI core. Claude Code, Codex, and Warp adapters will be added after model evaluation and end-to-end verification.
