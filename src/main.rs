use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use agent_shunt::{
    application::retrieve::RetrieveInput,
    composition::{Application, scan_input},
    config,
};

#[derive(Debug, Parser)]
#[command(
    name = "agent-shunt",
    version,
    about = "Read-only context worker for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze explicitly selected files with the configured worker model.
    Scan(ScanArgs),
    /// Select relevant source chunks with ripgrep; optionally analyze them.
    Retrieve(RetrieveArgs),
    /// Validate local configuration and credential discovery only.
    Check(ModelArgs),
    /// Check local dependencies without making a remote request.
    Doctor(ModelArgs),
    /// Show aggregate local metrics without source data.
    Metrics,
    /// Print recommended worker models with ready-to-use config fragments.
    Recommend,
    /// Install host integrations with backups and merge-safe configuration.
    Install {
        #[command(subcommand)]
        command: InstallCommand,
    },
    /// Internal lifecycle adapters used by host integrations.
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
}

#[derive(Debug, Subcommand)]
enum HookCommand {
    // 16 KiB (~4k tokens): the point where a bounded retrieval beats reading the
    // whole file. The installed hook command pins no flag, so this default is
    // what fires at runtime; 32 KiB only caught the largest ~5% of source files,
    // leaving the guardrail idle on most whole-file reads. Override per-invocation
    // with --threshold-bytes.
    CodexPreToolUse {
        #[arg(long, default_value_t = 16_384)]
        threshold_bytes: u64,
    },
    ClaudePreToolUse {
        #[arg(long, default_value_t = 16_384)]
        threshold_bytes: u64,
    },
}

#[derive(Debug, Subcommand)]
enum InstallCommand {
    Codex {
        /// Install the PreToolUse guardrail in addition to the skill.
        #[arg(long)]
        hook: bool,
        /// Codex home to install into; repeatable. Defaults to the codexHomes
        /// configuration value, or ~/.codex when unset.
        #[arg(long = "home")]
        homes: Vec<PathBuf>,
    },
    Claude {
        /// Install the PreToolUse guardrail in addition to the skill.
        #[arg(long)]
        hook: bool,
        /// Claude Code home to install into; repeatable. Defaults to the
        /// claudeHomes configuration value, or ~/.claude when unset.
        #[arg(long = "home")]
        homes: Vec<PathBuf>,
    },
    Gemini {
        /// Gemini CLI home to install into; repeatable. Defaults to
        /// ~/.gemini. The host has no hook surface: only the custom command
        /// is installed.
        #[arg(long = "home")]
        homes: Vec<PathBuf>,
    },
    Opencode {
        /// opencode home to install into; repeatable. Defaults to
        /// ~/.config/opencode. The host has no hook surface: only the skill
        /// is installed.
        #[arg(long = "home")]
        homes: Vec<PathBuf>,
    },
    /// Append the agent-shunt section to AGENTS.md in a repository root.
    AgentsMd {
        /// Repository root to install into. Defaults to the current
        /// directory.
        #[arg(long = "root", default_value = ".")]
        root: PathBuf,
    },
    /// Install the .cursor/rules/agent-shunt.mdc rule into a repository root.
    Cursor {
        /// Repository root to install into. Defaults to the current
        /// directory.
        #[arg(long = "root", default_value = ".")]
        root: PathBuf,
    },
    /// Install the .clinerules/agent-shunt.md rule into a repository root.
    Cline {
        /// Repository root to install into. Defaults to the current
        /// directory.
        #[arg(long = "root", default_value = ".")]
        root: PathBuf,
    },
    /// Install the .roo/rules/agent-shunt.md rule into a repository root.
    Roo {
        /// Repository root to install into. Defaults to the current
        /// directory.
        #[arg(long = "root", default_value = ".")]
        root: PathBuf,
    },
    /// Append the agent-shunt section to .github/copilot-instructions.md in
    /// a repository root.
    Copilot {
        /// Repository root to install into. Defaults to the current
        /// directory.
        #[arg(long = "root", default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Debug, Args)]
struct ModelArgs {
    #[arg(short, long)]
    model: Option<String>,
}

#[derive(Debug, Args)]
struct ScanArgs {
    #[arg(short, long)]
    question: String,
    #[arg(short, long, required = true)]
    path: Vec<PathBuf>,
    #[arg(short, long)]
    model: Option<String>,
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct RetrieveArgs {
    #[arg(short, long)]
    question: String,
    #[arg(long = "dir", default_value = ".")]
    cwd: PathBuf,
    #[arg(short, long)]
    model: Option<String>,
    #[arg(long, default_value_t = 12_000)]
    budget_tokens: usize,
    #[arg(long, default_value_t = 8)]
    context_lines: usize,
    #[arg(long, default_value_t = 200)]
    max_hits: usize,
    /// MMR relevance/diversity trade-off in [0,1]; higher favors relevance.
    /// Overrides config `mmrLambda`; defaults to the built-in tuning.
    #[arg(long = "mmr-lambda")]
    mmr_lambda: Option<f64>,
    /// Largest enclosing block a hit may expand into before falling back to the
    /// fixed context window. Overrides config `maxBlockLines`.
    #[arg(long = "max-block-lines")]
    max_block_lines: Option<usize>,
    /// Drop chunks scoring below this percent of the top hit. Overrides config
    /// `minScorePercent`.
    #[arg(long = "min-score-percent")]
    min_score_percent: Option<usize>,
    /// Ripgrep glob applied to the search (repeatable). Prefix with `!` to
    /// exclude, e.g. `--glob '!public/**'` or `--glob 'src/**'`.
    #[arg(long = "glob")]
    glob: Vec<String>,
    /// Exclude paths matching this glob (repeatable); sugar for `--glob '!<pat>'`.
    #[arg(long = "exclude")]
    exclude: Vec<String>,
    /// Restrict retrieval to locally changed files: tracked modifications
    /// against this base ref plus untracked files. Bare `--diff` uses HEAD
    /// (staged and unstaged changes).
    #[arg(long = "diff", num_args = 0..=1, default_missing_value = "HEAD")]
    diff: Option<String>,
    #[arg(long)]
    analyze: bool,
    /// Opt-in hybrid retrieval: fuse the local lexical ranking with a dense
    /// embedding ranking (Reciprocal Rank Fusion) so semantically relevant
    /// chunks surface even when they share few exact terms. Requires an
    /// `embeddingModel` in config and sends candidate chunks to that endpoint.
    #[arg(long)]
    semantic: bool,
    /// Opt-in precise re-ranking: the worker model scores the top candidate
    /// chunks for how directly they answer the question and reorders them
    /// before the budget is packed. Sends those chunks to the model.
    #[arg(long)]
    rerank: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("agent-shunt: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = Application::default();
    let value = match cli.command {
        Some(Command::Scan(args)) => {
            let config = config::load(args.model.as_deref())?;
            app.scan(
                &config,
                scan_input(args.question, args.path, args.cwd, &config),
                args.dry_run,
            )?
        }
        Some(Command::Retrieve(args)) => {
            let config = config::load(args.model.as_deref())?;
            let mut globs = args.glob;
            globs.extend(
                args.exclude
                    .into_iter()
                    .map(|pattern| format!("!{pattern}")),
            );
            use agent_shunt::application::retrieve::{
                MAX_BLOCK_LINES, MIN_SCORE_PERCENT, MMR_LAMBDA,
            };
            let input = RetrieveInput {
                question: args.question,
                cwd: args.cwd,
                limits: config.limits.clone(),
                budget_tokens: args.budget_tokens,
                context_lines: args.context_lines,
                max_hits: args.max_hits,
                globs,
                scope: args
                    .diff
                    .map(|base| agent_shunt::application::retrieve::ChangeScope { base }),
                // Precedence: explicit CLI flag, then config override, then the
                // built-in default.
                mmr_lambda: args.mmr_lambda.or(config.mmr_lambda).unwrap_or(MMR_LAMBDA),
                max_block_lines: args
                    .max_block_lines
                    .or(config.max_block_lines)
                    .unwrap_or(MAX_BLOCK_LINES),
                min_score_percent: args
                    .min_score_percent
                    .or(config.min_score_percent)
                    .unwrap_or(MIN_SCORE_PERCENT),
            };
            app.retrieve(input, args.analyze, args.semantic, args.rerank, &config)?
        }
        Some(Command::Check(args)) => {
            let config = config::load(args.model.as_deref())?;
            app.check(&config)?
        }
        Some(Command::Doctor(args)) => {
            let config = config::load(args.model.as_deref())?;
            app.doctor(&config)
        }
        Some(Command::Metrics) => app.metrics()?,
        Some(Command::Recommend) => app.recommend(),
        Some(Command::Install { command }) => match command {
            InstallCommand::Codex { hook, mut homes } => {
                if homes.is_empty() {
                    let config = config::load(None)?;
                    homes = if config.codex_homes.is_empty() {
                        let home = dirs::home_dir()
                            .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
                        vec![home.join(".codex")]
                    } else {
                        config.codex_homes
                    };
                }
                app.install_codex(&homes, hook)?
            }
            InstallCommand::Claude { hook, mut homes } => {
                if homes.is_empty() {
                    let config = config::load(None)?;
                    homes = if config.claude_homes.is_empty() {
                        let home = dirs::home_dir()
                            .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
                        vec![home.join(".claude")]
                    } else {
                        config.claude_homes
                    };
                }
                app.install_claude(&homes, hook)?
            }
            InstallCommand::Gemini { mut homes } => {
                if homes.is_empty() {
                    let home = dirs::home_dir()
                        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
                    homes = vec![home.join(".gemini")];
                }
                app.install_gemini(&homes, false)?
            }
            InstallCommand::Opencode { mut homes } => {
                if homes.is_empty() {
                    let home = dirs::home_dir()
                        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
                    homes = vec![home.join(".config").join("opencode")];
                }
                app.install_opencode(&homes, false)?
            }
            InstallCommand::AgentsMd { root } => {
                app.install_repo(agent_shunt::composition::RepoHost::AgentsMd, &root)?
            }
            InstallCommand::Cursor { root } => {
                app.install_repo(agent_shunt::composition::RepoHost::Cursor, &root)?
            }
            InstallCommand::Cline { root } => {
                app.install_repo(agent_shunt::composition::RepoHost::Cline, &root)?
            }
            InstallCommand::Roo { root } => {
                app.install_repo(agent_shunt::composition::RepoHost::Roo, &root)?
            }
            InstallCommand::Copilot { root } => {
                app.install_repo(agent_shunt::composition::RepoHost::Copilot, &root)?
            }
        },
        Some(Command::Hook { command }) => match command {
            HookCommand::CodexPreToolUse { threshold_bytes }
            | HookCommand::ClaudePreToolUse { threshold_bytes } => {
                agent_shunt::adapters::pre_tool_use::run_pre_tool_use(threshold_bytes)?;
                return Ok(());
            }
        },
        None => {
            Cli::command().print_help()?;
            println!();
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

use clap::CommandFactory;
