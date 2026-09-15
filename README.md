# agent-shunt

[![CI](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml/badge.svg)](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Stop dumping whole files into your agent's context.

Coding agents burn their context window reading files wholesale and exploring blindly. agent-shunt gives them just the relevant pieces — found locally, for free — and can optionally get a model to synthesize an answer where every claim is checked against the source that was actually sent.

Inspired by [Spotify's Portal "shunt" plugin](https://engineering.atspotify.com/2026/9/portal-by-spotify-cut-my-claude-code-token-usage-by-90), which showed how much token spend is just bulk reads — with two twists: retrieval here is local and free, and the worker's file/line references are validated locally instead of trusted.

```bash
agent-shunt retrieve --question "Where is authentication enforced?" --dir .
```

That's the core command. No API key, no network, nothing leaves your machine: it finds and ranks the relevant chunks of your codebase and returns them with line numbers, inside a token budget you choose.

## Two paths

**Local and free.** `retrieve` searches your code and returns ranked, line-numbered chunks within a strict token budget. A chunk that doesn't fit is not included — the budget is real.

**With a model, optional.** Add `--analyze` (or use `scan` for files you pick explicitly) and the selected evidence goes to a model for synthesis. The model's answer is accepted only if every file and line range it cites was actually part of the evidence sent. Hallucinated references are rejected locally, not trusted.

If every model in the chain fails, the command returns `host fallback required` instead of guessing — the calling agent just resumes its normal targeted reads.

## Install

You need `rg` (ripgrep) on your `PATH`, and access to any OpenAI-compatible endpoint if you want the model paths.

```bash
git clone https://github.com/jmtrs/agent-shunt.git
cd agent-shunt
cargo install --path .
```

Store your key once:

```bash
mkdir -p ~/.config/agent-shunt
echo 'AGENT_SHUNT_API_KEY=sk-...' > ~/.config/agent-shunt/.env
chmod 600 ~/.config/agent-shunt/.env
```

## Use any provider

Default is [OpenRouter](https://openrouter.ai) (`OPENROUTER_API_KEY` or `AGENT_SHUNT_API_KEY`). But there is no provider list in the code — any endpoint that serves `POST {baseUrl}/chat/completions`, accepts a Bearer token, and returns an OpenAI-shaped response works:

| Provider | `baseUrl` | Key |
| --- | --- | --- |
| OpenRouter (default) | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| OpenAI | `https://api.openai.com/v1` | `OPENAI_API_KEY` via `apiKeyEnv` |
| Groq | `https://api.groq.com/openai/v1` | `GROQ_API_KEY` via `apiKeyEnv` |
| Ollama (local) | `http://localhost:11434/v1` | none needed |
| LM Studio (local) | `http://localhost:1234/v1` | none needed |

…same for Together, Fireworks, Mistral, DeepSeek, xAI, Cerebras, self-hosted vLLM, or a LiteLLM proxy in front of anything. Ad-hoc:

```bash
export AGENT_SHUNT_BASE_URL="https://api.together.xyz/v1"
export AGENT_SHUNT_API_KEY="your-together-key"
```

Or persistent, using the provider's own variable name:

```json
{
  "baseUrl": "https://api.together.xyz/v1",
  "apiKeyEnv": "TOGETHER_API_KEY",
  "model": "deepseek-ai/DeepSeek-V3.1"
}
```

Model IDs follow your provider's naming. Local endpoints run keyless automatically. Set `responseFormat: "json_object"` if the provider lacks JSON-schema structured-output support — claim validation is local either way, so the no-hallucination guarantee doesn't depend on it.

## Commands

```bash
# Find relevant chunks (free, local)
agent-shunt retrieve --question "Where is auth enforced?" --dir .

# Same, then ask the model about exactly that evidence
agent-shunt retrieve --analyze --question "Where is auth enforced?" --dir .

# Analyze files you picked yourself
agent-shunt scan --question "Summarize this module" --path src/main.rs

# Diagnostics
agent-shunt check     # config + credential
agent-shunt doctor    # full local health report
agent-shunt metrics   # aggregate usage and cost
```

`retrieve` options: `--budget-tokens` (default 12000), `--context-lines` (8), `--max-hits` (200), `--model`.

## Privacy and safety

- **Read-only.** Never writes to your repository.
- **The model sees only the selected chunks**, never your whole repo.
- **On OpenRouter**, every request enforces Zero Data Retention routing, blocks data-collecting and `:free` routes. On any other provider, their data policy is between you and them.
- **Credentials never travel in cleartext**: plain `http` endpoints are limited to localhost and private networks. Redirects and environment proxies are ignored.
- **Your key stays local**: read at runtime, never printed, logged, or written to metrics. Metrics are aggregates only (status, model, timing, tokens, cost) — never questions, code, or answers.
- **Everything is bounded**: request and response sizes, token counts, file counts, timeouts.

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
  "disableReasoning": true,
  "extraBody": { "top_p": 0.1 },
  "codexHomes": ["~/.codex"],
  "claudeHomes": ["~/.claude"]
}
```

- `baseUrl` — any http(s) origin; env override `AGENT_SHUNT_BASE_URL`.
- Keys resolve in order: `apiKey` value → `AGENT_SHUNT_API_KEY` → `OPENROUTER_API_KEY` → the `apiKeyEnv` variable → env files.
- `model` / `fallbackModels` — your provider's IDs. Transport, HTTP, and validation failures advance through the chain under one overall timeout.
- `responseFormat` — `json_schema` (strict, when the provider supports it) or `json_object`. Output parsing is lenient in either mode: unknown keys are ignored, missing fields default, and a scalar where a list is expected is coerced — so loosely-conforming `json_object` providers still work, while claim validation stays local.
- `disableReasoning` — set `true` for reasoning models so they answer directly (this tool does grounded extraction, not deliberation). Auto-injects the provider's disable-thinking parameter: z.ai `thinking:{type:disabled}`, Qwen/DashScope `enable_thinking:false`, otherwise OpenRouter-style `reasoning:{enabled:false}`.
- `extraBody` — a JSON object merged into every request body, applied last so it overrides any tool default (including the reasoning field above). The escape hatch for any provider parameter the built-ins don't cover.
- Numeric knobs (`timeoutMs`, `maxOutputTokens`, size/file caps) are also accepted; defaults are sane.

CLI model overrides (`--model`) beat stored config.

### Recommended models

Run `agent-shunt recommend` for ready-to-merge config fragments. The task is grounded, structured extraction — cheap, fast, faithful line ranges — so reasoning adds nothing here.

| Model | `baseUrl` | Notes | Privacy |
| --- | --- | --- | --- |
| `deepseek/deepseek-v4-flash` | OpenRouter | Default. Fast, cheap, strict `json_schema`. | ZDR (no retention/training) |
| `z-ai/glm-4.7-flash` | OpenRouter | Default fallback; `json_schema`. | ZDR |
| `glm-5.3-flash` | `https://api.z.ai/api/coding/paas/v4` | Fast **only** with `disableReasoning: true`; use `json_object`. | none — source leaves to z.ai |
| local (Ollama/LM Studio/vLLM) | `http://localhost…` | Any capable instruct model, keyless; `json_object`. | full — nothing leaves the machine |

## Host integrations

Two hosts have first-class installers today:

```bash
agent-shunt install claude --hook   # Claude Code  (~/.claude)
agent-shunt install codex --hook    # Codex       (~/.codex)
```

Each drops a consultative skill into the host's skills directory and merges a fail-open guardrail into its hook config — `settings.json` for Claude Code, `hooks.json` for Codex. Whole-file reads above 32 KiB (`Read`/`Bash cat`) get redirected through the shunt with the reason inline; ranged reads and everything else pass untouched. Your existing settings, permissions, and hooks are preserved (a `.agent-shunt.bak` backup is made before any change), re-running is idempotent, and if any home fails, all homes roll back. Multiple homes via repeatable `--home` or the `codexHomes`/`claudeHomes` config keys. After a hook change, Codex asks you to re-trust it via `/hooks`; Claude Code needs nothing else.

**Any other agent.** The CLI is plain shell — any tool that can run commands (Gemini CLI, OpenCode, Cursor, …) can use it directly; paste the skill's two commands into its instructions. New hosts are a thin config on the same installer core, so more first-class integrations land without restructuring.

## License

[MIT](LICENSE)
