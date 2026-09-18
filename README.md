# agent-shunt

[![CI](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml/badge.svg)](https://github.com/jmtrs/agent-shunt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Find the code a question needs without filling your coding agent's context with whole files. `agent-shunt` returns ranked source excerpts with paths and line numbers, within a token budget. With default settings, retrieval runs locally, with no API key or network request.

Inspired by [Spotify's Portal "shunt" plugin](https://engineering.atspotify.com/2026/9/portal-by-spotify-cut-my-claude-code-token-usage-by-90), which showed the cost of bulk code reads in an agent's context. `agent-shunt` applies that idea through local retrieval and validates file and line references in optional model findings.

```bash
agent-shunt retrieve --question "Where is authentication enforced?" --dir .
```

Use it when you need to locate or understand code across a repository. If you already know the exact file and lines, a direct ranged read is simpler.

## Quick start

Install [ripgrep](https://github.com/BurntSushi/ripgrep) (`brew install ripgrep` on macOS or `sudo apt install ripgrep` on Ubuntu/Debian), then download the latest prebuilt binary:

```bash
curl -fsSL https://raw.githubusercontent.com/jmtrs/agent-shunt/main/install.sh -o install.sh
sh install.sh
~/.local/bin/agent-shunt retrieve --question "Where is authentication enforced?" --dir /path/to/your/repo
```

The installer supports macOS and Linux on Intel/AMD and ARM, verifies the download checksum, and puts the binary in `~/.local/bin` (set `AGENT_SHUNT_INSTALL_DIR` to choose another directory). It does not need Rust or a repository clone. If `~/.local/bin` is not on your `PATH`, the installer tells you. You can also [download a release asset directly](https://github.com/jmtrs/agent-shunt/releases/latest).

The result is JSON with `chunks[]`: each chunk has a path, line range, source text, and relevance score. `--budget-tokens` caps the estimated source context (default: 12,000). No configuration is needed for this command. To build from source instead, clone the repository and run `cargo install --path . --locked --features local-embed`.

## Choose a command

| What you need | Example |
| --- | --- |
| Find relevant code | `agent-shunt retrieve --question "Where is auth enforced?" --dir .` |
| Search only changed and untracked files | `agent-shunt retrieve --diff --question "What could this change break?" --dir .` |
| Find related code with different wording | `agent-shunt retrieve --semantic --question "Where is auth enforced?" --dir .` |
| Get an answer grounded in selected chunks | `agent-shunt retrieve --analyze --question "How does auth work?" --dir .` |
| Review a change for concrete risks | `agent-shunt retrieve --review --diff --question "Any risks?" --dir .` |
| Analyze a file you selected | `agent-shunt scan --path src/main.rs --question "Summarize this module"` |

`--diff` uses `HEAD` by default; pass a base ref such as `--diff origin/main` to review a larger change. It includes untracked files. `--review` implies `--analyze`.

Useful retrieval options:

| Option | When to use it |
| --- | --- |
| `--prf` | Add identifiers found in top results to a second local search. |
| `--expand` | Ask a chat model for related search terms before searching. |
| `--rerank` | Ask a chat model to reorder candidate chunks for precision. |
| `--why` | Show whether each chunk came from a term match or semantic recall. |
| `--glob 'src/**'`, `--exclude 'dist/**'` | Limit the files searched; both can be repeated. |
| `--budget-tokens 1200` | Return less source context. |

Options can be combined where useful, for example `--semantic --rerank` for broader recall followed by more precise ordering. Run `agent-shunt retrieve --help` for all flags and tuning controls.

## Optional semantic search

`--semantic` builds a persistent embedding index to recall code that shares few words with the question. The index caches vectors by file content and model, so later runs only re-embed changed files.

The prebuilt binary includes the ONNX backend. If you build from source, enable it explicitly:

```bash
cargo install --path . --locked --features local-embed
```

Add this to `~/.config/agent-shunt/config.json`:

```json
{
  "embeddingProvider": "local"
}
```

The default model is `bge-small-en-v1.5`; no chat provider, API key, or local server is needed. The first run downloads roughly 130 MB of model weights and builds the index on the CPU, which can take a few minutes for a large repository. Later searches reuse the model and index caches. If upgrading an existing installation, add `--force` to the `cargo install` command.

You can instead use an OpenAI-compatible `/embeddings` endpoint: set `embeddingProvider` to `"http"`, plus `embeddingBaseUrl`, `embeddingModel`, and, when needed, `embeddingApiKeyEnv` in the same config file. This mode sends candidate source chunks to that endpoint.

## Optional model analysis

`--analyze`, `--review`, `--rerank`, and `scan` use a chat model. The default chat provider is OpenRouter; set `OPENROUTER_API_KEY` or `AGENT_SHUNT_API_KEY`, or store the key in `~/.config/agent-shunt/.env`. For another OpenAI-compatible chat endpoint, set `baseUrl`, `model`, and `apiKeyEnv` in `config.json`:

```json
{
  "baseUrl": "https://your-provider.example/v1",
  "model": "your-model",
  "apiKeyEnv": "YOUR_API_KEY"
}
```

Run `agent-shunt recommend` for model suggestions and ready-to-use config fragments.

```bash
agent-shunt retrieve --analyze --question "How does auth work?" --dir .
agent-shunt scan --path src/main.rs --question "Summarize this module" --dry-run
```

The model sees selected evidence, not the whole repository. File and line references in structured findings are checked locally against that evidence; the model's explanation still needs source verification. If the configured models fail, the command reports that the calling agent must fall back to its own targeted reads.

## Integrate with a coding agent

Install instructions into a supported agent, or call the CLI directly from any agent that can run shell commands:

```bash
agent-shunt install codex            # Codex skill
agent-shunt install claude           # Claude Code skill
agent-shunt install gemini           # Gemini CLI command
agent-shunt install opencode         # opencode skill
agent-shunt install agents-md        # Managed section in this repo's AGENTS.md
```

Codex and Claude Code also accept `--hook` to redirect large whole-file reads to `agent-shunt`; the hook leaves ranged reads alone. Repository installers are available for `cursor`, `cline`, `roo`, and `copilot`. Installers preserve existing content, make backups before their first changes, and are safe to rerun. Use `agent-shunt install --help` for the full list and host-specific options.

## Privacy and limits

- Default `retrieve`, `--prf`, and `--why` are local. Local `--semantic` needs a one-time model download, then runs on your machine. HTTP `--semantic` sends candidate chunks to the embeddings endpoint.
- `--expand` sends the question to a chat model. `--analyze`, `--review`, `--rerank`, and `scan` send selected source to the configured model. Check the provider's data policy before using those modes with sensitive code.
- OpenRouter requests enforce Zero Data Retention routing. Other providers follow their own policies.
- Source references are validated, but that does not prove the model's claims. Review the cited lines before relying on an answer.
- Retrieval is read-only. Source-context token counts are estimates, not API billing tokens.

Use `agent-shunt check` for local configuration, `agent-shunt doctor` for dependency diagnostics, and `agent-shunt metrics` for aggregate usage without source content.

## Measured retrieval quality

On 50 human-authored questions across five pinned open-source repositories, all methods below had the same 1,200-token source-context budget:

| Method | Hit@1 | Hit@3 | Hit@5 | MRR | Avg. tokens/query |
| --- | ---: | ---: | ---: | ---: | ---: |
| Local semantic (`bge-small-en-v1.5`) | 50% | **82%** | **90%** | **0.662** | 1,047.8 |
| Default lexical retrieval | 50% | 76% | 84% | 0.619 | **1,026.1** |
| Targeted `rg` + fixed windows | 26% | 64% | 72% | 0.446 | 1,167.4 |
| Targeted `rg` + enclosing symbols | 26% | 64% | 72% | 0.445 | 1,171.8 |

Hit@k is the share of questions with a relevant source chunk in the first *k* results. MRR rewards relevant chunks appearing earlier; token counts estimate the source context delivered to the agent.

Against reading the same selected files in full, default retrieval used **94.37% less estimated source context** in this benchmark. These results measure retrieval, not end-to-end coding-agent performance; individual repositories vary. See the [methods, per-repository results, and caveats](eval/RESULTS.md) and [machine-readable snapshots](eval/results/).

## License

[MIT](LICENSE)
