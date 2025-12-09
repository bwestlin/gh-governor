use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::{error, info};

use gh_governor::config::{load_root_config, resolve_sets_dir};
use gh_governor::merge::merge_sets_for_repo;
use gh_governor::sets::SetDefinition;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Directory containing gh-governor-conf.(toml|yml|yaml|json) and config-sets/
    #[arg(long, default_value = ".")]
    config_base: PathBuf,

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
    /// Placeholder for applying changes (will create draft PRs)
    Apply {
        #[arg(long = "repo", value_name = "NAME")]
        repos: Vec<String>,
    },
}

fn main() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    match args.command {
        Command::Plan { repos } => run_plan(args.config_base, repos),
        Command::Apply { repos } => {
            run_plan(args.config_base, repos).context("apply not yet implemented; showing plan instead")
        }
    }
}

fn run_plan(config_base: PathBuf, only_repos: Vec<String>) -> Result<()> {
    let (root, root_path) = load_root_config(&config_base)?;
    info!("loaded config for org '{}' from {}", root.org, root_path.display());

    let sets_dir = resolve_sets_dir(&config_base, &root);
    let mut set_cache: HashMap<String, SetDefinition> = HashMap::new();

    for repo in root.repos.iter() {
        if !only_repos.is_empty() && !only_repos.contains(&repo.name) {
            continue;
        }

        let mut set_defs = Vec::new();
        for set_name in root
            .default_sets
            .iter()
            .chain(repo.sets.iter())
        {
            if !set_cache.contains_key(set_name) {
                let loaded = gh_governor::sets::load_set(&sets_dir, set_name)
                    .with_context(|| format!("loading set '{set_name}' for repo {}", repo.name))?;
                set_cache.insert(set_name.clone(), loaded);
            }
            let cached = set_cache
                .get(set_name)
                .expect("set should be loaded")
                .clone();
            set_defs.push(cached);
        }

        if set_defs.is_empty() {
            info!("repo '{}' has no configuration sets assigned", repo.name);
            continue;
        }

        match merge_sets_for_repo(&set_defs) {
            Ok(merged) => {
                info!(
                    "repo '{}': {} labels, {} templates, repo_settings={}, branch_protection={}",
                    repo.name,
                    merged.labels.len(),
                    merged.issue_templates.len(),
                    merged.repo_settings.is_some(),
                    merged.branch_protection.is_some()
                );
            }
            Err(err) => {
                error!("repo '{}': merge failed: {err}", repo.name);
            }
        }
    }

    if let Some(defaults) = &root.org_defaults {
        info!(
            "org defaults: {} labels defined for new repos",
            defaults.labels.len()
        );
    }

    Ok(())
}
