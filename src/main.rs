use std::path::PathBuf;

use clap::{Parser, Subcommand};

use gh_governor::app::{Mode, run};
use gh_governor::config::{load_root_config, resolve_sets_dir};
use gh_governor::error::Result;
use gh_governor::github::GithubClient;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Directory containing gh-governor-conf.(toml|yml|yaml|json) and config-sets/
    #[arg(long, default_value = ".")]
    config_base: PathBuf,

    /// GitHub token (or set env GITHUB_TOKEN)
    #[arg(
        long,
        env = "GITHUB_TOKEN",
        value_name = "TOKEN",
        hide_env_values = true
    )]
    token: String,

    /// Show extra details for blocked label removals
    #[arg(long, short = 'v')]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Validate and show the merged configuration for repos (dry-run)
    Plan {
        /// Limit to specific repositories; if omitted, all repos in config are used
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
    },
    /// Apply changes (creates/updates labels and settings)
    Apply {
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    let (mode, only_repos) = match args.command {
        Command::Plan { repos } => (Mode::Plan, repos),
        Command::Apply { repos } => (Mode::Apply, repos),
    };
    let (root, root_path) = load_root_config(&args.config_base)?;
    let sets_dir = resolve_sets_dir(&args.config_base, &root);
    let gh = GithubClient::new(&args.token, root.org.clone())?;

    run(
        mode,
        root,
        root_path,
        sets_dir,
        only_repos,
        gh,
        args.verbose,
    )
    .await
}
