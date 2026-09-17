# agent-shunt

[![CI](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml/badge.svg)](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Stop dumping whole files into your agent's context.

Coding agents burn their context window reading files wholesale and exploring blindly. agent-shunt gives them just the relevant pieces — found locally, for free — and can optionally get a model to synthesize an answer whose structured findings are checked against the source references that were actually sent.

Inspired by [Spotify's Portal "shunt" plugin](https://engineering.atspotify.com/2026/9/portal-by-spotify-cut-my-claude-code-token-usage-by-90), which showed how much token spend is just bulk reads — with two twists: retrieval here is local and free, and the worker's file/line references are validated locally instead of trusted.

```bash
agent-shunt retrieve --question "Where is authentication enforced?" --dir .
```

That's the core command. No API key, no network, nothing leaves your machine: it finds and ranks the relevant chunks of your codebase and returns them with line numbers, inside a token budget you choose.

## Two paths

**Local and free.** `retrieve` searches your code and returns ranked, line-numbered chunks within a strict token budget. A chunk that doesn't fit is not included — the budget is real. Chunks snap to their enclosing block instead of a fixed line window, near-duplicate and padding lines are dropped, and no single file is allowed to flood the budget — so the evidence stays dense and on-target.

**With a model, optional.** Add `--analyze` (or use `scan` for files you pick explicitly) and the selected evidence goes to a model for synthesis. Each structured finding is accepted only when its file and line range are inside the evidence sent; findings with unknown or out-of-context references are dropped locally. The finding summary itself is still model output, so source-reference validation is not a proof that every semantic claim is true.

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

Model IDs follow your provider's naming. Local endpoints run keyless automatically. Set `responseFormat: "json_object"` if the provider lacks JSON-schema structured-output support — source-reference validation is local either way, so cited paths and line ranges are checked independently of provider-side schema enforcement.

## Commands

```bash
# Find relevant chunks (free, local)
agent-shunt retrieve --question "Where is auth enforced?" --dir .

# Same, then ask the model about exactly that evidence
agent-shunt retrieve --analyze --question "Where is auth enforced?" --dir .

# Opt-in query expansion: a chat model adds related search terms (login,
# session, token…) so the local search finds code phrased differently. Works
# with any chat provider — no embeddings endpoint needed.
agent-shunt retrieve --expand --question "Where is auth enforced?" --dir .

# Opt-in hybrid retrieval: a persistent embedding index recalls chunks from
# across the whole repo by meaning, fused with the local lexical ranking, so
# relevant code surfaces even from files the term search never hit. Needs an
# embeddingModel in config; vectors are cached on disk per file + model.
agent-shunt retrieve --semantic --question "Where is auth enforced?" --dir .

# Opt-in precise re-ranking: the worker model scores the top candidates for
# how directly they answer the question and reorders them. Combine with
# --semantic for broad recall then precise reranking.
agent-shunt retrieve --semantic --rerank --question "Where is auth enforced?" --dir .

# Opt-in pseudo-relevance feedback: mine distinctive identifiers from the
# top-ranked files and fold them back into the search, so the origin symbol
# the question never named still surfaces. Fully local — no provider.
agent-shunt retrieve --prf --question "Why is the wrong column filtered?" --dir .

# Show why each chunk was chosen (lexical term match vs dense recall, and the
# matched terms), so a surprising result is legible instead of opaque.
agent-shunt retrieve --why --question "Where is auth enforced?" --dir .

# Opt-in review: critique the diff for risks, broken invariants, and missing
# edge cases (severity per finding) instead of just answering. Implies --analyze.
agent-shunt retrieve --review --diff --question "Any risks in these changes?" --dir .

# Analyze files you picked yourself
agent-shunt scan --question "Summarize this module" --path src/main.rs

# Diagnostics
agent-shunt check     # config + credential
agent-shunt doctor    # full local health report
agent-shunt metrics   # aggregate usage and cost
```

`retrieve` options: `--budget-tokens` (default 12000), `--context-lines` (8), `--max-hits` (200), `--model`, `--expand`, `--prf`, `--semantic`, `--rerank`, `--why`, `--review`. Retrieval tuning: `--mmr-lambda`, `--max-block-lines`, `--min-score-percent` (also `mmrLambda`/`maxBlockLines`/`minScorePercent` config keys; CLI wins).

Four opt-in ways to widen recall beyond the exact term — pick by what your provider offers (`--prf` needs nothing):

- **`--prf`** — pseudo-relevance feedback: a first lexical pass finds the leader files, then their distinctive compound identifiers (a camelCase hump or an underscore, common in a couple of the leaders but not all) are folded into the search. Recovers the origin symbol a natural-language question omits — e.g. the field or helper that turns out to be the real cause — without you naming it. Fully local: one extra ripgrep pass and a bounded read, no provider.
- **`--expand`** — a chat model suggests related terms (synonyms, likely identifiers) that widen the lexical search. Cheapest network option, needs only a chat model (works with OpenRouter). Set `expandByDefault: true` in config to make it *your* default (the shipped default stays local).
- **`--semantic`** — a persistent, disk-cached embedding index over the whole repository recalls code the term search missed, fused with the lexical ranking (RRF). Higher quality. Embeds either through a provider that serves `/embeddings` (`embeddingProvider: "http"`, the default) **or fully on-device with no key or network** (`embeddingProvider: "local"`, a keyless ONNX model — build with `--features local-embed`). Vectors cache under `~/.cache/agent-shunt/index`, keyed by file content and model, so only changed files are re-embedded.
- **`--rerank`** — the worker model scores the top candidates for how directly they answer the question and reorders them. Precision stage; combine with `--expand`/`--semantic` for the standard recall-then-rerank design.

`--expand`, `--semantic`, and `--rerank` send content to a provider; `--prf` and the default `retrieve` stay fully local (no network, no key).

Two more flags make results legible and turn retrieval into review:

- **`--why`** — annotates each chunk with its `source` (`lexical` for a term match, `dense` for a chunk the semantic index recalled by meaning) and the query terms it matched. A chunk that shares none of your words came from dense recall, not your query — the annotation says so, so a surprising rank is explained rather than eroding trust.
- **`--review`** (implies `--analyze`) — swaps the analyst prompt ("answer the question") for a reviewer prompt: surface concrete risks, broken invariants, unhandled edge cases, and unsafe assumptions, each with a `severity` (`high`/`medium`/`low`) and a fix. Pair with `--diff` to review exactly what you changed.

**Reviewing new or changed code?** Use `--diff` to scope retrieval to exactly what you touched — tracked modifications **and untracked new files** — against a base ref (bare `--diff` uses HEAD):

```bash
agent-shunt retrieve --diff --question "what did this change break?" --dir .
agent-shunt retrieve --diff origin/main --question "review these changes" --dir .
```

Even without `--diff`, plain `retrieve` gives a modest ranking boost to matching files that are git-untracked (brand-new), so freshly written code is not buried under older files that share its vocabulary.

Chunks snap to their enclosing definition — the function, method, or class with its signature, decorators, and doc-comments — via tree-sitter (Rust, Python, JS/TS/TSX, Go, Java, C/C++, Ruby, Bash, JSON), falling back to a language-agnostic indentation heuristic elsewhere. Build with `--no-default-features` to drop tree-sitter and use the heuristic everywhere.

## Privacy and safety

- **Read-only.** Never writes to your repository.
- **`retrieve` is fully local by default**: no network, no API key. `--analyze`, `scan`, and the opt-in `--semantic` pass are the only paths that reach a provider.
- **The model sees only the selected chunks**, never your whole repo. `--semantic` sends candidate chunks to your embeddings endpoint and `--rerank` sends the top candidates to the worker model, each under its data policy; the default retrieval never leaves your machine.
- **Model output remains untrusted.** Structured findings are source-reference validated: their path and line range must fall inside evidence that was actually supplied. This catches invented or out-of-context references; it does not prove that a model-written summary is semantically correct.
- **On OpenRouter**, every request — worker and embeddings alike — enforces Zero Data Retention routing and blocks data-collecting and `:free` routes. On any other provider, their data policy is between you and them.
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
  "embeddingModel": "text-embedding-3-small",
  "embeddingBaseUrl": "https://api.openai.com/v1",
  "embeddingApiKeyEnv": "OPENAI_API_KEY",
  "embeddingProvider": "http",
  "codexHomes": ["~/.codex"],
  "claudeHomes": ["~/.claude"]
}
```

- `baseUrl` — any http(s) origin; env override `AGENT_SHUNT_BASE_URL`.
- Keys resolve in order: `apiKey` value → `AGENT_SHUNT_API_KEY` → `OPENROUTER_API_KEY` → the `apiKeyEnv` variable → env files.
- `model` / `fallbackModels` — your provider's IDs. Transport, HTTP, and validation failures advance through the chain under one overall timeout.
- `responseFormat` — `json_schema` (strict, when the provider supports it) or `json_object`. Output parsing is lenient in either mode: unknown keys are ignored, missing fields default, and a scalar where a list is expected is coerced — so loosely-conforming `json_object` providers still work, while source-reference validation stays local.
- `disableReasoning` — set `true` for reasoning models so they answer directly (this tool does grounded extraction, not deliberation). Auto-injects the provider's disable-thinking parameter: z.ai `thinking:{type:disabled}`, Qwen/DashScope `enable_thinking:false`, otherwise OpenRouter-style `reasoning:{enabled:false}`.
- `extraBody` — a JSON object merged into every request body, applied last so it overrides any tool default (including the reasoning field above). The escape hatch for any provider parameter the built-ins don't cover.
- `embeddingModel` — enables `--semantic`. Point `embeddingBaseUrl` at a provider that serves `/embeddings` (OpenAI, or a local Ollama/LM Studio) — **OpenRouter does not**, so `embeddingBaseUrl` usually differs from `baseUrl` even though it defaults to it. The key resolves like the worker's, or via `embeddingApiKeyEnv`. A local endpoint (Ollama `nomic-embed-text`) keeps `--semantic` keyless and on-machine.
- `embeddingProvider` — `"http"` (default) uses the `/embeddings` client above; `"local"` runs a keyless, on-device model with no network at query time (`embeddingModel` defaults to `bge-small-en-v1.5`; also `bge-base-en-v1.5`, `bge-large-en-v1.5`, `all-minilm-l6-v2`, `nomic-embed-text-v1.5`). The local backend is compiled in only with `--features local-embed`, which pulls the ONNX Runtime and downloads the model weights once to a cache on first use; the default build stays lean and selecting `"local"` without the feature fails with a build hint.

  **Local backend cost.** The `local-embed` build is ~15 MB larger (bundles the ONNX Runtime); the `bge-small-en-v1.5` weights are a one-time ~130 MB download to `.fastembed_cache/`. Embedding runs on the CPU: building the index the first time is the expensive step — a few minutes on a large tree — and holds a few hundred MB of RAM while it runs. After that, per-file vectors are cached on disk (~50 KB/file) and only changed files are re-embedded, so warm queries return in well under a second. On a large repo, scope the first build with `--glob` so you only index what the question is about.
- `expandByDefault` — set `true` to apply `--expand` (LLM query expansion) on every `retrieve`. Off by default so the shipped default stays local; the CLI `--expand` flag enables it per-invocation regardless.
- Retrieval tuning `mmrLambda` / `maxBlockLines` / `minScorePercent` override the built-in defaults; the matching CLI flags win over config.
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

Skill-based hosts (instructions installed into the agent's home):

```bash
agent-shunt install claude --hook    # Claude Code  (~/.claude): skill + guardrail hook
agent-shunt install codex --hook     # Codex       (~/.codex):  skill + guardrail hook
agent-shunt install gemini           # Gemini CLI  (~/.gemini): /agent-shunt custom command
agent-shunt install opencode         # opencode    (~/.config/opencode): skill
```

Claude Code and Codex each merge a fail-open guardrail into their hook config — `settings.json` and `hooks.json` respectively. Whole-file reads above 32 KiB (`Read`/`Bash cat`) get redirected through the shunt with the reason inline; ranged reads and everything else pass untouched. After a hook change, Codex asks you to re-trust it via `/hooks`; after a Gemini command install, run `/commands reload`. Multiple homes via repeatable `--home` (Claude/Codex also honor the `claudeHomes`/`codexHomes` config keys).

Repo-based hosts (instruction files installed into a repository root, default `.`):

```bash
agent-shunt install agents-md        # AGENTS.md (Codex, Amp, Zed, Droid, Jules, …)
agent-shunt install cursor           # .cursor/rules/agent-shunt.mdc
agent-shunt install cline            # .clinerules/agent-shunt.md
agent-shunt install roo              # .roo/rules/agent-shunt.md
agent-shunt install copilot          # .github/copilot-instructions.md
```

`AGENTS.md` and `.github/copilot-instructions.md` usually hold your own content, so the installer appends a managed section between `agent-shunt` markers — re-running replaces only that section, never your lines. The Cursor/Cline/Roo rule files are ours alone and land next to your existing rules.

Every installer is transactional: your existing files are preserved (a `.agent-shunt.bak` backup sits beside any file before it is first touched), re-running is idempotent, and any failure rolls back all writes.

**Any other agent.** The CLI is plain shell — any tool that can run commands can use it directly; paste the skill's two commands into its instructions. New hosts are a thin config on the same installer core, so more first-class integrations land without restructuring.

## License

[MIT](LICENSE)
