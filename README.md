# agent-shunt

[![CI](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml/badge.svg)](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Provider-neutral, read-only context worker for coding agents, implemented in Rust.

Coding agents burn context on whole-file reads and broad exploration. `agent-shunt` gives them bounded, validated context instead: deterministic local retrieval for free, and an optional model pass for synthesis — with every model claim checked against the source that was actually sent.

```
agent-shunt retrieve --question "Where is authentication enforced?" --dir .
```

## How it works

Two independent paths:

- **`retrieve`** — finds and ranks bounded source chunks locally with `rg`. No model request, no network, no credentials. Git ignore rules are respected and common generated directories are excluded. Search is streamed with a hard hit cap, a 10-second process deadline, and a 5 MiB per-candidate search cap. The evidence token budget is strict: a chunk that does not fit is not included.
- **`scan` and `retrieve --analyze`** — send explicit or retrieved context to any OpenAI-compatible chat-completions endpoint and validate every returned path and line range locally. A finding is accepted only when its complete line range was actually included in the evidence sent to the model.

When every model in the chain fails, the command returns `host fallback required` instead of guessing, so the calling agent resumes its normal targeted reads.

## Security and privacy model

- **Read-only.** The tool never writes to your repository.
- **Sandboxed file access.** Files are opened component-by-component with `openat` beneath an already-open root directory descriptor. Symlinked path components are rejected, inputs must be strict UTF-8 regular files, and file metadata must remain stable while reading.
- **Transport guardrail.** The worker endpoint is yours to choose, but plain `http` is only accepted for loopback and private-network hosts — credentials are never sent in cleartext to a remote provider. URLs embedding credentials, queries, or fragments are rejected.
- **Zero Data Retention on OpenRouter.** When the endpoint host is `openrouter.ai`, every model request requires structured-output support, denies data-collection providers, enforces Zero Data Retention routing, and blocks `:free` routes. Other providers receive no proprietary fields; their data policy is between you and them.
- **Bounded everything.** Request, response, question, output-token, file-count, per-file and total-byte limits are all enforced before anything is sent or accepted.
- **Credentials stay local.** The API key is read at runtime from the environment or a config file and is never printed, copied, or written to metrics.
- **Aggregate-only metrics.** Local telemetry records operation status, model, timing, byte/file counts, token usage, cost, and fallback state. Questions, paths, source content, answers, and credentials are never stored.

## Providers

`agent-shunt` speaks the OpenAI-compatible chat-completions protocol and ships no provider of its own — point it at whichever endpoint you already use:

| Provider | `baseUrl` | Key |
| --- | --- | --- |
| [OpenRouter](https://openrouter.ai) (default) | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| OpenAI | `https://api.openai.com/v1` | `OPENAI_API_KEY` via `apiKeyEnv` |
| Groq | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` via `apiKeyEnv` |
| Ollama (local) | `http://localhost:11434/v1` | none needed |
| LM Studio (local) | `http://localhost:1234/v1` | none needed |

Local endpoints run keyless automatically. Set `responseFormat` to `"json_object"` for providers without JSON-schema structured-output support.

### Any other provider

The table above is examples, not an allowlist — there is no provider list in the code. Any endpoint that (1) serves `POST {baseUrl}/chat/completions`, (2) accepts `Authorization: Bearer <key>`, and (3) returns an OpenAI-shaped response works: Together, Fireworks, Mistral, DeepSeek direct, xAI, Cerebras, SambaNova, self-hosted vLLM, or a LiteLLM proxy in front of a non-OpenAI API.

Ad-hoc, no config file needed:

```bash
export AGENT_SHUNT_BASE_URL="https://api.together.xyz/v1"
export AGENT_SHUNT_API_KEY="your-together-key"
agent-shunt retrieve --analyze --question "..." --dir .
```

Or persistent, with the provider's own variable name:

```json
{
  "baseUrl": "https://api.together.xyz/v1",
  "apiKeyEnv": "TOGETHER_API_KEY",
  "model": "deepseek-ai/DeepSeek-V3.1",
  "fallbackModels": [],
  "responseFormat": "json_object"
}
```

Model IDs follow the provider's own naming, not OpenRouter slugs. Set `fallbackModels` to `[]` when the provider offers a single suitable model.

## Install

Requirements:

- Rust 1.85+ (edition 2024)
- [`rg`](https://github.com/BurntSushi/ripgrep) (Ripgrep) on your `PATH` — required for `retrieve`
- Access to any OpenAI-compatible endpoint — only for the model-backed paths (`scan`, `retrieve --analyze`)

```bash
git clone https://github.com/jmtrs/agent-shunt.git
cd agent-shunt
cargo install --path .
```

Provide the key via `AGENT_SHUNT_API_KEY` (or `OPENROUTER_API_KEY`) in the environment, or store it once:

```bash
mkdir -p ~/.config/agent-shunt
echo 'AGENT_SHUNT_API_KEY=sk-...' > ~/.config/agent-shunt/.env
chmod 600 ~/.config/agent-shunt/.env
```

## Usage

### Deterministic retrieval (no model, no key)

```bash
agent-shunt retrieve \
  --question "Where is authentication enforced?" \
  --dir . \
  --budget-tokens 12000
```

Returns ranked, line-numbered source chunks within a strict token budget. Adjacent hits are merged into one chunk, but never beyond the budget: a file with many scattered matches is split into budget-sized chunks so the top-ranked file is not starved by a single oversized range. Options: `--context-lines` (default 8), `--max-hits` (default 200), `--model` for later analysis.

### Retrieval + analysis

```bash
agent-shunt retrieve --analyze \
  --question "Where is authentication enforced?" \
  --dir .
```

Sends only the selected line ranges to the configured worker model; model findings are accepted only when their line ranges were part of the sent evidence.

### Explicit scan

```bash
# Analyze explicitly chosen files
agent-shunt scan \
  --question "Summarize this module" \
  --path src/domain/mod.rs \
  --path src/application/scan.rs

# Validate and size inputs without credentials or network
agent-shunt scan --dry-run --question "Summarize this module" --path src/domain/mod.rs
```

### Diagnostics

```bash
agent-shunt check     # local configuration and credential check
agent-shunt doctor    # health report, including ripgrep availability
agent-shunt metrics   # aggregate usage and cost telemetry
```

## Codex integration

```bash
agent-shunt install codex --hook
```

Installs a consultative skill plus a fail-open `PreToolUse` guardrail into every configured Codex home. Homes resolve in this order: repeatable `--home <path>` CLI flags, then the `codexHomes` configuration value, then `~/.codex`. The installer:

- creates backups before touching `hooks.json`,
- preserves unrelated hooks and merges idempotently,
- rolls back all homes if any install step fails.

After a hook change, open `/hooks` in Codex and trust the reviewed definition. Whole-file reads above 32 KiB are redirected through the shunt; ranged reads and unknown command shapes remain untouched.

## Configuration

Optional `~/.config/agent-shunt/config.json`:

```json
{
  "baseUrl": "https://openrouter.ai/api/v1",
  "model": "deepseek/deepseek-v4-flash",
  "fallbackModels": ["z-ai/glm-4.7-flash"],
  "apiKey": "sk-...",
  "apiKeyEnv": "GROQ_API_KEY",
  "responseFormat": "json_schema",
  "codexHomes": ["~/.codex", "~/.codex-work"],
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

- `baseUrl` — any absolute http(s) OpenAI-compatible origin (env override: `AGENT_SHUNT_BASE_URL`). Plain `http` is limited to loopback and private-network hosts.
- `apiKey` — key stored directly in the config file (mind its permissions).
- `apiKeyEnv` — name of an environment variable holding the key, for provider-specific names like `GROQ_API_KEY`. Resolution order: `apiKey` config value, `AGENT_SHUNT_API_KEY`, `OPENROUTER_API_KEY`, the `apiKeyEnv` variable, then the env files.
- `responseFormat` — `"json_schema"` (default) or `"json_object"` for providers without schema support.
- `codexHomes` — accepts `~`-relative paths, defaults to `["~/.codex"]`.

CLI model overrides take precedence over stored configuration.

### Model chain and fallback

The default chain is DeepSeek V4 Flash → GLM 4.7 Flash on OpenRouter; set `model` and `fallbackModels` to your provider's IDs when pointing elsewhere. Transport, HTTP, empty-output, invalid-JSON, schema, and source-reference failures advance through the chain; one overall timeout covers the complete chain. If every model fails, the command returns `host fallback required`.

## Architecture

Hexagonal (ports and adapters), dependency pointing inward:

```
domain
  ← application use cases and owned ports
      ← outbound adapters (filesystem, rg, OpenAI-compatible worker, metrics, credentials)
          ← composition root
              ← CLI
```

- `src/domain/` — values and invariants, no infrastructure knowledge.
- `src/application/` — use cases and consumer-owned port traits.
- `src/adapters/` — filesystem, ripgrep, OpenAI-compatible worker, credential, and metrics implementations, plus the Codex installer and hook.
- `src/composition.rs` — dependency wiring and operation-level metrics.
- `src/main.rs` — thin CLI adapter.

## Development

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

`rg` on the `PATH` is required for the ripgrep adapter tests. The test suite includes a deterministic eight-case retrieval gate (`tests/retrieval_evaluation.rs`) that runs against a fixture tree with a strict evidence budget.

## License

[MIT](LICENSE)
