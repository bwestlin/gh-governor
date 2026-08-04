use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use gh_governor::app::Mode;
use gh_governor::app::PullRequestOptions;
use gh_governor::app::RunOptions;
use gh_governor::app::run_with_options;
use gh_governor::config::load_root_config;
use gh_governor::config::resolve_sets_dir;
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
        #[command(flatten)]
        pull_request: PullRequestArgs,
    },
    /// Apply changes (creates/updates labels and settings)
    Apply {
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
        /// Directory containing gh-governor-conf.(toml|yml|yaml|json) and config-sets/
        #[arg(long, default_value = ".")]
        config_base: PathBuf,
        #[command(flatten)]
        pull_request: PullRequestArgs,
    },
    /// Generate config files from existing repositories
    Generate {
        /// Repositories to harvest (at least one required)
        #[arg(long = "repos", value_name = "NAME[,NAME...]", value_delimiter = ',')]
        repos: Vec<String>,
        /// GitHub organization to read from
        #[arg(long)]
        org: String,
        /// Output directory for generated configuration (defaults to ./generated-conf-<org>)
        #[arg(long)]
        output: Option<PathBuf>,
        /// Format for generated configuration files (toml|yml|json)
        #[arg(long, value_enum, default_value = "toml")]
        format: OutputFormatArg,
    },
}

#[derive(clap::Args, Clone, Debug, Default)]
struct PullRequestArgs {
    /// Title for created or existing gh-governor pull requests
    #[arg(long, value_name = "TITLE")]
    pr_title: Option<String>,
    /// Body/message for created or existing gh-governor pull requests
    #[arg(long, value_name = "MESSAGE")]
    pr_message: Option<String>,
    /// Desired draft state for created or existing gh-governor pull requests
    #[arg(long, value_name = "BOOL")]
    pr_draft: Option<bool>,
}

impl From<PullRequestArgs> for PullRequestOptions {
    fn from(args: PullRequestArgs) -> Self {
        Self {
            title: args.pr_title,
            message: args.pr_message,
            draft: args.pr_draft,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormatArg {
    Toml,
    Yml,
    Json,
}

impl From<OutputFormatArg> for gh_governor::generate::OutputFormat {
    fn from(val: OutputFormatArg) -> Self {
        match val {
            OutputFormatArg::Toml => gh_governor::generate::OutputFormat::Toml,
            OutputFormatArg::Yml => gh_governor::generate::OutputFormat::Yml,
            OutputFormatArg::Json => gh_governor::generate::OutputFormat::Json,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    match run_command(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run_command(args: Args) -> Result<()> {
    match args.command {
        Command::Plan {
            repos,
            config_base,
            pull_request,
        } => {
            let (root, root_path) = load_root_config(&config_base)?;
            let sets_dir = resolve_sets_dir(&config_base, &root);
            let gh = GithubClient::new(&args.token, root.org.clone())?;
            run_with_options(
                Mode::Plan,
                root,
                root_path,
                sets_dir,
                repos,
                gh,
                RunOptions {
                    pull_request: pull_request.into(),
                    verbose: args.verbose,
                },
            )
            .await
        }
        Command::Apply {
            repos,
            config_base,
            pull_request,
        } => {
            let (root, root_path) = load_root_config(&config_base)?;
            let sets_dir = resolve_sets_dir(&config_base, &root);
            let gh = GithubClient::new(&args.token, root.org.clone())?;
            run_with_options(
                Mode::Apply,
                root,
                root_path,
                sets_dir,
                repos,
                gh,
                RunOptions {
                    pull_request: pull_request.into(),
                    verbose: args.verbose,
                },
            )
            .await
        }
        Command::Generate {
            repos,
            org,
            output,
            format,
        } => {
            if repos.is_empty() {
                return Err(gh_governor::error::Error::InvalidArgs(
                    "generate requires at least one --repo".to_string(),
                ));
            }
            let gh = GithubClient::new(&args.token, org.clone())?;
            let output_dir =
                output.unwrap_or_else(|| PathBuf::from(format!("./generated-conf-{org}")));
            gh_governor::generate::generate_configs(
                &gh,
                &repos,
                &output_dir,
                &org,
                args.verbose,
                format.into(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pull_request_options_for_apply() {
        let args = Args::try_parse_from([
            "gh-governor",
            "--token",
            "token",
            "apply",
            "--pr-title",
            "Managed updates",
            "--pr-message",
            "Changes managed by gh-governor",
            "--pr-draft",
            "false",
        ])
        .unwrap();

        let Command::Apply { pull_request, .. } = args.command else {
            panic!("expected apply command");
        };
        assert_eq!(pull_request.pr_title.as_deref(), Some("Managed updates"));
        assert_eq!(
            pull_request.pr_message.as_deref(),
            Some("Changes managed by gh-governor")
        );
        assert_eq!(pull_request.pr_draft, Some(false));
    }
}
