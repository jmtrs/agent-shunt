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
    CodexPreToolUse {
        #[arg(long, default_value_t = 32_768)]
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
    #[arg(long)]
    analyze: bool,
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
            let input = RetrieveInput {
                question: args.question,
                cwd: args.cwd,
                limits: config.limits.clone(),
                budget_tokens: args.budget_tokens,
                context_lines: args.context_lines,
                max_hits: args.max_hits,
            };
            app.retrieve(input, args.analyze, &config)?
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
        },
        Some(Command::Hook { command }) => match command {
            HookCommand::CodexPreToolUse { threshold_bytes } => {
                agent_shunt::adapters::codex_hook::run_pre_tool_use(threshold_bytes)?;
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
