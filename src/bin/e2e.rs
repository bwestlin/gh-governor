use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};

use gh_governor::config::{RepoConfig, RootConfig, load_root_config, resolve_sets_dir};
use gh_governor::error::{Error, Result};
use gh_governor::github::GithubClient;
use gh_governor::merge::merge_sets_for_repo;
use gh_governor::sets::SetDefinition;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// GitHub organization to use for tests
    #[arg(long)]
    org: String,

    /// Config base for plan/apply
    #[arg(long)]
    config_base: PathBuf,

    /// Directory for logs
    #[arg(long, default_value = "target/e2e-logs")]
    logs: PathBuf,

    #[command(subcommand)]
    command: E2eCommand,
}

#[derive(Subcommand, Debug)]
enum E2eCommand {
    /// Run the full workflow
    Run,
    /// Delete created repositories
    Cleanup,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = env::var("GITHUB_TOKEN").map_err(|_| {
        Error::InvalidArgs("GITHUB_TOKEN is not set in the environment".to_string())
    })?;

    let gh = GithubClient::new(&token, args.org.clone())?;

    match args.command {
        E2eCommand::Run => run_flow(&gh, &args).await,
        E2eCommand::Cleanup => {
            let repos = load_repo_names(&args.config_base)?;
            cleanup(&gh, &repos).await
        }
    }
}

async fn run_flow(gh: &GithubClient, args: &Args) -> Result<()> {
    fs::create_dir_all(&args.logs)?;
    log(&args.logs, "Starting e2e run");

    let repos = load_repo_names(&args.config_base)?;
    create_repos(gh, &repos, &args.logs).await?;
    seed_repos(gh, &repos, &args.logs).await?;

    let plan_log = args.logs.join("plan.log");
    run_governor(&args.logs, "plan", &args.config_base, &plan_log)?;

    let apply_log = args.logs.join("apply.log");
    run_governor(&args.logs, "apply", &args.config_base, &apply_log)?;

    verify_state(gh, &args.config_base, &repos, &args.logs).await?;
    log(&args.logs, "E2E run complete");
    Ok(())
}

async fn cleanup(gh: &GithubClient, repos: &[String]) -> Result<()> {
    for repo in repos {
        gh.delete_repo(repo).await?;
    }
    Ok(())
}

async fn create_repos(gh: &GithubClient, repos: &[String], logs: &Path) -> Result<()> {
    for repo in repos {
        log(logs, &format!("Creating repo {}", repo));
        gh.create_repo(repo, false).await?;
    }
    Ok(())
}

async fn seed_repos(gh: &GithubClient, repos: &[String], logs: &Path) -> Result<()> {
    for repo in repos {
        log(logs, &format!("Seeding repo {}", repo));
        let readme = gh.get_file(repo, "README.md", None).await?;
        if readme.is_none() {
            gh.put_file(
                repo,
                "README.md",
                &format!("Test repo {}\n", repo),
                None,
                "Initial commit",
                None,
            )
            .await?;
        }
    }
    Ok(())
}

fn run_governor(
    logs: &Path,
    mode: &str,
    config_base: &Path,
    log_path: &Path,
) -> Result<()> {
    log(logs, &format!("Running gh-governor {} for config {}", mode, config_base.display()));
    let mut cmd = Command::new("cargo");
    cmd.args(["run", "--quiet", "--bin", "gh-governor", "--"])
        .arg("-v")
        .arg(mode)
        .arg("--config-base")
        .arg(config_base);
    let output = cmd
        .output()
        .map_err(|e| Error::io_with_path(e, log_path.into()))?;
    let mut combined = Vec::new();
    combined.extend_from_slice(&output.stdout);
    combined.extend_from_slice(&output.stderr);
    fs::write(log_path, combined)?;
    Ok(())
}

fn load_repo_names(config_base: &Path) -> Result<Vec<String>> {
    let (root, _) = load_root_config(config_base)?;
    let repos: Vec<String> = root.repos.iter().map(|repo| repo.name.clone()).collect();
    if repos.is_empty() {
        return Err(Error::InvalidArgs(format!(
            "no repositories defined in config at {}",
            config_base.display()
        )));
    }
    Ok(repos)
}

async fn verify_state(
    gh: &GithubClient,
    config_base: &Path,
    repos: &[String],
    logs: &Path,
) -> Result<()> {
    log(logs, "Verifying state via GitHub API");
    let (root, _) = load_root_config(config_base)?;
    let sets_dir = resolve_sets_dir(config_base, &root);
    let merged = prepare_merged(&root, &sets_dir, repos)?;

    for (repo_name, cfg) in merged {
        log(logs, &format!("Verifying {}", repo_name));
        let labels = gh.list_repo_labels(&repo_name).await?;
        let label_names: Vec<String> = labels.iter().map(|l| l.name.clone()).collect();
        for label in &cfg.labels {
            if !label_names.iter().any(|n| n == &label.name) {
                log(
                    logs,
                    &format!("Missing label {} on {}", label.name, repo_name),
                );
            }
        }

        if let Some(bp_cfg) = cfg
            .repo_settings
            .as_ref()
            .and_then(|s| s.branch_protection.as_ref())
        {
            for rule in &bp_cfg.rules {
                let current = gh.get_branch_protection(&repo_name, &rule.pattern).await?;
                if current.is_none() {
                    log(
                        logs,
                        &format!(
                            "Missing branch protection {} on {}",
                            rule.pattern, repo_name
                        ),
                    );
                }
            }
        }

        if !cfg.github_files.is_empty() {
            let pr = gh
                .find_open_pr_by_head_prefix(&repo_name, "gh-governor/updates-", "main")
                .await?;
            if pr.is_none() {
                log(
                    logs,
                    &format!("Missing PR for .github updates in {}", repo_name),
                );
            }
        }
    }

    Ok(())
}

fn prepare_merged(
    root: &RootConfig,
    sets_dir: &Path,
    only_repos: &[String],
) -> Result<Vec<(String, gh_governor::merge::MergedRepoConfig)>> {
    let mut set_cache: HashMap<String, SetDefinition> = HashMap::new();
    let mut merged = Vec::new();

    for repo in root.repos.iter() {
        if !only_repos.is_empty() && !only_repos.contains(&repo.name) {
            continue;
        }
        let set_defs = collect_sets(root, repo, &mut set_cache, sets_dir)?;
        if set_defs.is_empty() {
            continue;
        }
        let merged_cfg = merge_sets_for_repo(&set_defs).map_err(|err| Error::MergeConflict {
            repo: repo.name.clone(),
            reason: err.to_string(),
        })?;
        merged.push((repo.name.clone(), merged_cfg));
    }

    Ok(merged)
}

fn collect_sets(
    root: &RootConfig,
    repo: &RepoConfig,
    cache: &mut HashMap<String, SetDefinition>,
    sets_dir: &Path,
) -> Result<Vec<SetDefinition>> {
    let mut set_defs = Vec::new();
    for set_name in root.default_sets.iter().chain(repo.sets.iter()) {
        if !cache.contains_key(set_name) {
            let loaded = gh_governor::sets::load_set(sets_dir, set_name)?;
            cache.insert(set_name.clone(), loaded);
        }
        let cached = cache.get(set_name).expect("set should be loaded").clone();
        set_defs.push(cached);
    }
    Ok(set_defs)
}

fn log(dir: &Path, msg: &str) {
    let line = format!("[e2e] {}\n", msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!("{}", msg);
}
