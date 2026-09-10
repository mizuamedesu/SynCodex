mod codex;
mod storage;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Back up, merge and restore Codex conversation history"
)]
struct Cli {
    #[arg(long, env = "CODEX_HOME", global = true)]
    codex_home: Option<PathBuf>,
    /// SQLite directory; defaults to the selected Codex home
    #[arg(long, env = "CODEX_SQLITE_HOME", global = true)]
    sqlite_home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a snapshot including conversation databases via SQLite backup API
    Dump {
        output: PathBuf,
        /// Exclude conversation databases
        #[arg(long)]
        history_only: bool,
    },
    /// Restore new files and merge compatible history. Stop destination Codex first.
    Inject {
        input: PathBuf,
        #[arg(long)]
        dry_run: bool,
        /// Keep destination databases and request Codex to rebuild its history index
        #[arg(long)]
        history_only: bool,
    },
    /// List the physical rollouts in a snapshot without printing conversation content
    List { input: PathBuf },
    /// Verify restored history using the real Codex app-server (no model turn is sent)
    Verify {
        thread_id: String,
        #[arg(long, default_value = "codex")]
        codex: String,
        /// Require this text to appear in restored message items; repeatable
        #[arg(long)]
        expect_text: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = cli
        .codex_home
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
        .context("Set --codex-home or CODEX_HOME")?;
    let sqlite_home = cli.sqlite_home.unwrap_or_else(|| home.clone());
    match cli.command {
        Command::Dump {
            output,
            history_only,
        } => storage::dump(&home, &sqlite_home, &output, history_only),
        Command::Inject {
            input,
            dry_run,
            history_only,
        } => storage::inject(&input, &home, &sqlite_home, dry_run, history_only),
        Command::List { input } => storage::list(&input),
        Command::Verify {
            thread_id,
            codex,
            expect_text,
        } => codex::verify(&home, &sqlite_home, &codex, &thread_id, &expect_text),
    }
}
