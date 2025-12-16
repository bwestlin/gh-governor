use std::path::PathBuf;

use clap::{Parser, Subcommand};

use gh_governor::app::{Mode, run};
use gh_governor::config::{load_root_config, resolve_sets_dir};
use gh_governor::error::Result;
use gh_governor::github::GithubClient;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
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
        /// Directory containing gh-governor-conf.(toml|yml|yaml|json) and config-sets/
        #[arg(long, default_value = ".")]
        config_base: PathBuf,
    },
    /// Apply changes (creates/updates labels and settings)
    Apply {
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
        /// Directory containing gh-governor-conf.(toml|yml|yaml|json) and config-sets/
        #[arg(long, default_value = ".")]
        config_base: PathBuf,
    },
    /// Generate config files from existing repositories
    Generate {
        /// Repositories to harvest (at least one required)
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
        /// GitHub organization to read from
        #[arg(long)]
        org: String,
        /// Output directory for generated configuration (defaults to ./generated-conf-<org>)
        #[arg(long)]
        output: Option<PathBuf>,
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
    match args.command {
        Command::Plan { repos, config_base } => {
            let (root, root_path) = load_root_config(&config_base)?;
            let sets_dir = resolve_sets_dir(&config_base, &root);
            let gh = GithubClient::new(&args.token, root.org.clone())?;
            run(
                Mode::Plan,
                root,
                root_path,
                sets_dir,
                repos,
                gh,
                args.verbose,
            )
            .await
        }
        Command::Apply { repos, config_base } => {
            let (root, root_path) = load_root_config(&config_base)?;
            let sets_dir = resolve_sets_dir(&config_base, &root);
            let gh = GithubClient::new(&args.token, root.org.clone())?;
            run(
                Mode::Apply,
                root,
                root_path,
                sets_dir,
                repos,
                gh,
                args.verbose,
            )
            .await
        }
        Command::Generate { repos, org, output } => {
            if repos.is_empty() {
                return Err(gh_governor::error::Error::InvalidArgs(
                    "generate requires at least one --repo".to_string(),
                ));
            }
            let gh = GithubClient::new(&args.token, org.clone())?;
            let output_dir =
                output.unwrap_or_else(|| PathBuf::from(format!("./generated-conf-{org}")));
            gh_governor::generate::generate_configs(&gh, &repos, &output_dir, &org, args.verbose)
                .await
        }
    }
}
