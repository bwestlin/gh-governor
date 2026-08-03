use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use clap::Parser;
use clap::Subcommand;
use gh_governor::config::RepoConfig;
use gh_governor::config::RootConfig;
use gh_governor::config::load_root_config;
use gh_governor::config::resolve_sets_dir;
use gh_governor::error::Error;
use gh_governor::error::Result;
use gh_governor::github::GithubClient;
use gh_governor::merge::merge_sets_for_repo;
use gh_governor::sets::SetDefinition;
use owo_colors::OwoColorize;
use tokio::time::sleep;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// GitHub organization to use for tests
    #[arg(long)]
    org: String,

    /// GitHub token (falls back to GITHUB_TOKEN)
    #[arg(long, env = "GITHUB_TOKEN")]
    token: String,

    /// Config base for plan/apply
    #[arg(long)]
    config_base: PathBuf,

    /// Directory for logs
    #[arg(long, default_value = "target/e2e-logs")]
    logs: PathBuf,

    /// Show detailed output from steps
    #[arg(short, long)]
    verbose: bool,

    /// Continue running steps after a failure (still exits non-zero at end)
    #[arg(long)]
    continue_on_fail: bool,

    /// Cleanup repos after run (use --no-cleanup to disable)
    #[arg(long, default_value_t = true)]
    cleanup: bool,

    /// Build gh-governor before running (use --no-build to disable)
    #[arg(long, default_value_t = true)]
    build: bool,

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
    if args.token.trim().is_empty() {
        return Err(Error::InvalidArgs(
            "GITHUB_TOKEN is not set in the environment or via --token".to_string(),
        ));
    }

    let gh = GithubClient::new(&args.token, args.org.clone())?;

    match args.command {
        E2eCommand::Run => run_flow(&gh, &args).await,
        E2eCommand::Cleanup => {
            let repos = load_repo_names(&args.config_base)?;
            cleanup(&gh, &repos, &args.logs).await
        }
    }
}

const PR_BRANCH_PREFIX: &str = "gh-governor/updates-";

async fn run_flow(gh: &GithubClient, args: &Args) -> Result<()> {
    fs::create_dir_all(&args.logs)?;
    log_status(&args.logs, StepStatus::Info, "Starting e2e run");
    if args.verbose {
        let options = format!(
            "Options: {}{}{}",
            if args.verbose { "--verbose " } else { "" },
            if args.continue_on_fail {
                "--continue-on-fail "
            } else {
                ""
            },
            if args.cleanup {
                "--cleanup"
            } else {
                "--no-cleanup"
            }
        );
        log(&args.logs, options.trim_end());
    }

    let repos = load_repo_names(&args.config_base)?;
    let oauth_scopes = gh.oauth_scopes().await?;
    let has_workflow_files = has_workflow_files(&args.config_base, &repos)?;
    let mut last_err: Option<Error> = None;
    let mut summary = RunSummary::default();
    let mut proceed = true;

    if args.build {
        log_status(&args.logs, StepStatus::Info, "Building gh-governor");
        if let Err(err) = build_governor(&args.logs, args.verbose) {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Build failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(&args.logs, StepStatus::Ok, "Build complete");
            summary.passed += 1;
        }
    }

    if proceed {
        log_status(&args.logs, StepStatus::Info, "Creating repos");
        if let Err(err) = create_repos(gh, &repos, &args.logs).await {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Create repos failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(&args.logs, StepStatus::Ok, "Repos ready");
            summary.passed += 1;
        }
    }

    if proceed {
        log_status(&args.logs, StepStatus::Info, "Seeding repos");
        if let Err(err) = seed_repos(gh, &repos, &args.logs).await {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Seed repos failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(&args.logs, StepStatus::Ok, "Repos seeded");
            summary.passed += 1;
        }
    }

    let plan_log = args.logs.join("plan.log");
    if proceed {
        log_status(&args.logs, StepStatus::Info, "Running plan");
        if let Err(err) = run_governor(
            &args.logs,
            "plan",
            &args.config_base,
            &plan_log,
            args.verbose,
            &args.token,
        ) {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Plan failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(&args.logs, StepStatus::Ok, "Plan complete");
            summary.passed += 1;
        }
    }

    let apply_log = args.logs.join("apply.log");
    if proceed {
        log_status(&args.logs, StepStatus::Info, "Running apply");
        if let Err(err) = run_governor(
            &args.logs,
            "apply",
            &args.config_base,
            &apply_log,
            args.verbose,
            &args.token,
        ) {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Apply failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(&args.logs, StepStatus::Ok, "Apply complete");
            summary.passed += 1;
        }
    }

    if proceed && has_workflow_files {
        log_status(
            &args.logs,
            StepStatus::Info,
            "Checking workflow scope warning",
        );
        if let Err(err) =
            check_workflow_scope_warning(&args.logs, &plan_log, &apply_log, oauth_scopes.as_ref())
        {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Workflow scope warning check failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
            if !args.continue_on_fail {
                proceed = false;
            }
        } else {
            log_status(
                &args.logs,
                StepStatus::Ok,
                "Workflow scope warning check complete",
            );
            summary.passed += 1;
        }
    }

    if proceed {
        log_status(&args.logs, StepStatus::Info, "Verifying state");
        if let Err(err) = verify_state(
            gh,
            &args.config_base,
            &repos,
            &args.logs,
            oauth_scopes.as_ref(),
        )
        .await
        {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Verification failed: {}", err),
            );
            last_err = Some(err);
            summary.failed += 1;
        } else {
            log_status(&args.logs, StepStatus::Ok, "Verification complete");
            summary.passed += 1;
        }
    }
    log_status(&args.logs, StepStatus::Ok, "E2E run complete");
    if args.cleanup {
        log_status(&args.logs, StepStatus::Info, "Cleanup after run");
        if let Err(err) = cleanup(gh, &repos, &args.logs).await {
            log_status(
                &args.logs,
                StepStatus::Fail,
                &format!("Cleanup failed: {}", err),
            );
            if !args.continue_on_fail {
                return Err(err);
            }
            last_err = Some(err);
            summary.failed += 1;
        } else {
            log_status(&args.logs, StepStatus::Ok, "Cleanup complete");
            summary.passed += 1;
        }
    }
    log_summary(&args.logs, summary.passed, summary.failed);
    last_err.map_or(Ok(()), Err)
}

async fn cleanup(gh: &GithubClient, repos: &[String], logs: &Path) -> Result<()> {
    let mut last_err: Option<Error> = None;
    let mut failed_existing = Vec::new();
    for repo in repos {
        match gh.get_repo(repo).await {
            Ok(_) => match gh.delete_repo(repo).await {
                Ok(()) => {
                    if confirm_repo_deleted(gh, repo).await? {
                        log(logs, &format!("Deleted repo {}", repo));
                    } else {
                        let msg = format!(
                            "Delete requested for {}, but repo still exists (check permissions/org)",
                            repo
                        );
                        log(logs, &msg);
                        failed_existing.push(repo.clone());
                    }
                }
                Err(err) => {
                    let hint = cleanup_error_hint(&err);
                    let msg = if let Some(hint) = hint {
                        format!("Failed to delete {}: {} ({})", repo, err, hint)
                    } else {
                        format!("Failed to delete {}: {}", repo, err)
                    };
                    log(logs, &msg);
                    last_err = Some(err);
                }
            },
            Err(Error::RepoNotFound { .. }) => {
                log(logs, &format!("Repo {} not found, skipping", repo));
            }
            Err(err) => {
                let hint = cleanup_error_hint(&err);
                let msg = if let Some(hint) = hint {
                    format!("Failed to check repo {}: {} ({})", repo, err, hint)
                } else {
                    format!("Failed to check repo {}: {}", repo, err)
                };
                log(logs, &msg);
                last_err = Some(err);
            }
        }
    }
    if !failed_existing.is_empty() {
        return Err(Error::InvalidArgs(format!(
            "cleanup failed for {} repo(s) that still exist: {}. Ensure the token has the 'delete_repo' scope (classic PAT) or repo admin permissions with delete rights.",
            failed_existing.len(),
            failed_existing.join(", ")
        )));
    }
    last_err.map_or(Ok(()), Err)
}

fn cleanup_error_hint(err: &Error) -> Option<&'static str> {
    match err {
        Error::Octo(err) => {
            if let octocrab::Error::GitHub { source, .. } = err.as_ref()
                && (source.status_code == http::StatusCode::FORBIDDEN
                    || source.status_code == http::StatusCode::UNAUTHORIZED)
            {
                return Some("token likely missing 'delete_repo' scope");
            }
            None
        }
        _ => None,
    }
}

async fn confirm_repo_deleted(gh: &GithubClient, repo: &str) -> Result<bool> {
    for _ in 0..3 {
        match gh.get_repo(repo).await {
            Ok(_) => sleep(Duration::from_millis(500)).await,
            Err(Error::RepoNotFound { .. }) => return Ok(true),
            Err(err) => return Err(err),
        }
    }
    Ok(false)
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
    verbose: bool,
    token: &str,
) -> Result<()> {
    log(
        logs,
        &format!(
            "Running gh-governor {} for config {}",
            mode,
            config_base.display()
        ),
    );
    let governor_path = governor_path()?;
    if !governor_path.exists() {
        return Err(Error::InvalidArgs(format!(
            "gh-governor binary not found at {} (run `cargo build --bin gh-governor` first)",
            governor_path.display()
        )));
    }

    let args = vec![
        "-v".to_string(),
        mode.to_string(),
        "--config-base".to_string(),
        config_base.display().to_string(),
    ];
    let cmd_line = format!("{} {}", governor_path.display(), args.join(" "));
    let mut cmd = Command::new(&governor_path);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !token.trim().is_empty() {
        cmd.env("GITHUB_TOKEN", token);
    }

    let status = run_command(logs, log_path, verbose, cmd, &cmd_line)?;

    if !status.success() {
        return Err(Error::InvalidArgs(format!(
            "gh-governor {} failed with status {}",
            mode, status
        )));
    }
    Ok(())
}

fn build_governor(logs: &Path, verbose: bool) -> Result<()> {
    let governor_path = governor_path()?;
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--bin").arg("gh-governor");
    sanitize_cargo_env(&mut cmd);
    if governor_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("release")
    {
        cmd.arg("--release");
    }
    let cmd_line = format!(
        "cargo {}",
        cmd.get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let log_path = logs.join("build.log");
    let status = run_command(logs, &log_path, verbose, cmd, &cmd_line)?;
    if !status.success() {
        return Err(Error::InvalidArgs(format!(
            "build failed with status {}",
            status
        )));
    }
    if !governor_path.exists() {
        return Err(Error::InvalidArgs(format!(
            "gh-governor binary not found at {} after build",
            governor_path.display()
        )));
    }
    Ok(())
}

fn governor_path() -> Result<PathBuf> {
    Ok(std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("gh-governor")))
        .unwrap_or_else(|| PathBuf::from("target").join("debug").join("gh-governor")))
}

fn run_command(
    logs: &Path,
    log_path: &Path,
    verbose: bool,
    mut cmd: Command,
    cmd_line: &str,
) -> Result<std::process::ExitStatus> {
    if verbose {
        log(logs, &format!("  Command: {}", cmd_line));
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::io_with_path(e, log_path.into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::InvalidArgs("failed to capture stdout from command".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::InvalidArgs("failed to capture stderr from command".to_string()))?;

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(log_path)?;
    let file = Arc::new(Mutex::new(file));

    let file_out = Arc::clone(&file);
    let logs_out = logs.to_path_buf();
    let out_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(|line| line.ok()) {
            if let Ok(mut f) = file_out.lock() {
                let _ = writeln!(f, "stdout | {}", line);
            }
            if verbose {
                log(&logs_out, &format!("  stdout | {}", line));
            }
        }
    });

    let file_err = Arc::clone(&file);
    let logs_err = logs.to_path_buf();
    let err_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(|line| line.ok()) {
            if let Ok(mut f) = file_err.lock() {
                let _ = writeln!(f, "stderr | {}", line);
            }
            if verbose {
                log(&logs_err, &format!("  stderr | {}", line));
            }
        }
    });

    let status = child
        .wait()
        .map_err(|e| Error::io_with_path(e, log_path.into()))?;
    let _ = out_handle.join();
    let _ = err_handle.join();
    Ok(status)
}

fn sanitize_cargo_env(cmd: &mut Command) {
    let preserve = ["CARGO_HOME", "CARGO_TARGET_DIR"];
    for (key, _value) in std::env::vars() {
        if key.starts_with("CARGO_") && !preserve.contains(&key.as_str()) {
            cmd.env_remove(key);
        }
    }
    for key in preserve {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
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

fn has_workflow_files(config_base: &Path, repos: &[String]) -> Result<bool> {
    let (root, _) = load_root_config(config_base)?;
    let sets_dir = resolve_sets_dir(config_base, &root);
    let merged = prepare_merged(&root, &sets_dir, repos)?;
    for (_repo_name, cfg) in merged {
        if cfg
            .github_files
            .iter()
            .any(|file| short_github_path(&file.path).starts_with(".github/workflows/"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn check_workflow_scope_warning(
    logs: &Path,
    plan_log: &Path,
    apply_log: &Path,
    scopes: Option<&Vec<String>>,
) -> Result<()> {
    let warning = "Warning: token missing 'workflow' scope; .github/workflows updates will fail";
    let Some(scopes) = scopes else {
        log(
            logs,
            "Skipping workflow scope warning check (X-OAuth-Scopes not available)",
        );
        return Ok(());
    };
    let has_workflow_scope = scopes.iter().any(|scope| scope == "workflow");
    let plan_text = fs::read_to_string(plan_log)?;
    let apply_text = fs::read_to_string(apply_log)?;
    let saw_warning = plan_text.contains(warning) || apply_text.contains(warning);
    if has_workflow_scope && saw_warning {
        return Err(Error::InvalidArgs(
            "workflow warning present despite token having 'workflow' scope".to_string(),
        ));
    }
    if !has_workflow_scope && !saw_warning {
        return Err(Error::InvalidArgs(
            "missing workflow warning despite token lacking 'workflow' scope".to_string(),
        ));
    }
    Ok(())
}

async fn verify_state(
    gh: &GithubClient,
    config_base: &Path,
    repos: &[String],
    logs: &Path,
    oauth_scopes: Option<&Vec<String>>,
) -> Result<()> {
    log(logs, "Verifying state via GitHub API");
    let (root, _) = load_root_config(config_base)?;
    let sets_dir = resolve_sets_dir(config_base, &root);
    let merged = prepare_merged(&root, &sets_dir, repos)?;
    let mut failures = 0u64;
    let mut failed_repos: Vec<String> = Vec::new();

    for (repo_name, cfg) in merged {
        log_repo_header(logs, &format!("Repo {}", repo_name));
        let repo_info = gh.get_repo(&repo_name).await?;
        let base_branch = repo_info
            .default_branch
            .clone()
            .unwrap_or_else(|| "main".to_string());

        log_repo_line(
            logs,
            &format!("Check labels ({} expected)", cfg.labels.len()),
        );
        let labels = gh.list_repo_labels(&repo_name).await?;
        let label_names: Vec<String> = labels.iter().map(|l| l.name.clone()).collect();
        let mut repo_failed = false;
        for label in &cfg.labels {
            if !label_names.iter().any(|n| n == &label.name) {
                log_repo_line(
                    logs,
                    &format!("Missing label {} on {}", label.name, repo_name),
                );
                log_repo_sub(logs, &format!("expected label: {}", label.name));
                failures += 1;
                repo_failed = true;
            }
        }

        let bp_count = cfg
            .repo_settings
            .as_ref()
            .and_then(|s| s.branch_protection.as_ref())
            .map(|cfg| cfg.rules.len())
            .unwrap_or(0);
        log_repo_line(
            logs,
            &format!("Check branch protection ({} expected)", bp_count),
        );
        if let Some(bp_cfg) = cfg
            .repo_settings
            .as_ref()
            .and_then(|s| s.branch_protection.as_ref())
        {
            for rule in &bp_cfg.rules {
                let current = gh.get_branch_protection(&repo_name, &rule.pattern).await?;
                if current.is_none() {
                    log_repo_line(
                        logs,
                        &format!(
                            "Missing branch protection {} on {}",
                            rule.pattern, repo_name
                        ),
                    );
                    log_repo_sub(logs, &format!("expected branch: {}", rule.pattern));
                    failures += 1;
                    repo_failed = true;
                }
            }
        }

        log_repo_line(
            logs,
            &format!(
                "Check .github updates ({} expected file(s))",
                cfg.github_files.len()
            ),
        );
        if cfg.github_files.is_empty() {
            log_repo_line(logs, "No .github files configured; skipping PR check");
        } else {
            let has_workflow_files = cfg
                .github_files
                .iter()
                .any(|file| short_github_path(&file.path).starts_with(".github/workflows/"));
            let missing_workflow_scope = has_workflow_files
                && oauth_scopes
                    .map(|scopes| !scopes.iter().any(|scope| scope == "workflow"))
                    .unwrap_or(false);
            if missing_workflow_scope {
                log_repo_line(
                    logs,
                    "Note: token missing 'workflow' scope; workflow updates may fail",
                );
            }

            let mut missing_files = Vec::new();
            for file in &cfg.github_files {
                if gh
                    .get_file(&repo_name, &file.path, Some(&base_branch))
                    .await?
                    .is_none()
                {
                    missing_files.push(file.path.clone());
                }
            }
            let needs_pr = !missing_files.is_empty();
            if needs_pr {
                log_repo_line(
                    logs,
                    &format!("Base branch missing: {}", missing_files.join(", ")),
                );
            } else {
                log_repo_line(logs, "Base branch missing: none");
            }

            let existing_pr = gh
                .find_open_pr_by_head_prefix(&repo_name, PR_BRANCH_PREFIX, &base_branch)
                .await?;
            if let Some(pr) = &existing_pr {
                let owner = repo_info
                    .owner
                    .as_ref()
                    .map(|o| o.login.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                let url = pr
                    .html_url
                    .as_ref()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| {
                        format!(
                            "https://github.com/{}/{}/pull/{}",
                            owner, repo_name, pr.number
                        )
                    });
                log_repo_line(
                    logs,
                    &format!(
                        "PR: #{} {} ({} -> {})",
                        pr.number, url, pr.head.ref_field, base_branch
                    ),
                );
            } else {
                log_repo_line(logs, "PR: none");
            }

            let branch_name = existing_pr
                .as_ref()
                .map(|pr| pr.head.ref_field.clone())
                .unwrap_or_else(|| format!("{}{}", PR_BRANCH_PREFIX, base_branch));
            let branch_exists = gh
                .get_branch_sha_optional(&repo_name, &branch_name)
                .await?
                .is_some();
            log_repo_line(
                logs,
                &format!(
                    "PR branch {}: {}",
                    branch_name,
                    if branch_exists { "exists" } else { "missing" }
                ),
            );

            if needs_pr {
                if missing_workflow_scope {
                    log_repo_line(
                        logs,
                        "Note: token missing 'workflow' scope; workflow updates may fail",
                    );
                }
                if existing_pr.is_none() {
                    log_repo_line(
                        logs,
                        &format!("Missing PR for .github updates in {}", repo_name),
                    );
                    log_repo_sub(
                        logs,
                        &format!("expected branch: {}{}", PR_BRANCH_PREFIX, base_branch),
                    );
                    failures += 1;
                    repo_failed = true;
                }
                if !branch_exists {
                    log_repo_line(
                        logs,
                        &format!("Missing update branch {} for {}", branch_name, repo_name),
                    );
                    log_repo_sub(logs, &format!("expected branch: {}", branch_name));
                    failures += 1;
                    repo_failed = true;
                }
                if let Some(pr) = &existing_pr {
                    let expected_files: HashSet<String> = cfg
                        .github_files
                        .iter()
                        .map(|file| short_github_path(&file.path))
                        .collect();
                    let actual_files: HashSet<String> = gh
                        .list_github_files(&repo_name, &pr.head.ref_field, ".github/")
                        .await?
                        .into_iter()
                        .collect();
                    let mut missing = expected_files
                        .difference(&actual_files)
                        .cloned()
                        .collect::<Vec<_>>();
                    let mut extra = actual_files
                        .difference(&expected_files)
                        .cloned()
                        .collect::<Vec<_>>();
                    missing.sort();
                    extra.sort();
                    if missing.is_empty() && extra.is_empty() {
                        log_repo_line(logs, "PR .github files: match expected");
                    } else {
                        log_repo_line(logs, "PR .github files: mismatch");
                        if !missing.is_empty() {
                            log_repo_line(
                                logs,
                                &format!("PR missing files: {}", missing.join(", ")),
                            );
                            for path in &missing {
                                log_repo_sub(
                                    logs,
                                    &format!("missing in PR: {}", short_github_path(path)),
                                );
                            }
                        }
                        if !extra.is_empty() {
                            log_repo_line(logs, &format!("PR extra files: {}", extra.join(", ")));
                            for path in &extra {
                                log_repo_sub(
                                    logs,
                                    &format!("extra in PR: {}", short_github_path(path)),
                                );
                            }
                        }
                        failures += (missing.len() + extra.len()) as u64;
                        repo_failed = true;
                    }
                }
            } else if existing_pr.is_some() {
                log_repo_line(
                    logs,
                    &format!("Unexpected PR exists for .github updates in {}", repo_name),
                );
                log_repo_sub(
                    logs,
                    &format!("unexpected PR with branch prefix: {}", PR_BRANCH_PREFIX),
                );
                failures += 1;
                repo_failed = true;
            }
        }

        if repo_failed {
            failed_repos.push(repo_name);
        }
    }

    if failures > 0 {
        return Err(Error::InvalidArgs(format!(
            "verification failed: {} missing expectation(s) across repo(s): {}. See e2e.log for details.",
            failures,
            failed_repos.join(", ")
        )));
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
    for set_name in root.sets_for_repo(repo) {
        if !cache.contains_key(set_name) {
            let loaded = gh_governor::sets::load_set(sets_dir, set_name)?;
            cache.insert(set_name.clone(), loaded);
        }
        let cached = match cache.get(set_name) {
            Some(value) => value.clone(),
            None => {
                return Err(Error::InvalidArgs(format!(
                    "missing set '{}' after loading",
                    set_name
                )));
            }
        };
        set_defs.push(cached);
    }
    Ok(set_defs)
}

fn short_github_path(path: &str) -> String {
    if let Some(idx) = path.find(".github/") {
        path[idx..].to_string()
    } else {
        path.to_string()
    }
}

fn log(dir: &Path, msg: &str) {
    let line = format!("  {}\n", msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!("  {}", msg);
}

fn log_repo_header(dir: &Path, msg: &str) {
    let line = format!("  {}\n", msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!("  {}", msg);
}

fn log_repo_line(dir: &Path, msg: &str) {
    let line = format!("    {}\n", msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!("    {}", msg);
}

fn log_repo_sub(dir: &Path, msg: &str) {
    let line = format!("      {}\n", msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!("      {}", msg);
}

#[derive(Copy, Clone)]
enum StepStatus {
    Info,
    Ok,
    Fail,
}

#[derive(Default)]
struct RunSummary {
    passed: u64,
    failed: u64,
}

fn log_status(dir: &Path, status: StepStatus, msg: &str) {
    let label = match status {
        StepStatus::Info => "INFO",
        StepStatus::Ok => "OK",
        StepStatus::Fail => "FAIL",
    };
    let line = format!("[{}] {}\n", label, msg);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    let colored = match status {
        StepStatus::Info => label.cyan().to_string(),
        StepStatus::Ok => label.green().to_string(),
        StepStatus::Fail => label.red().to_string(),
    };
    if matches!(status, StepStatus::Info) {
        println!("Step - {}", msg);
    } else {
        println!("[{}] {}", colored, msg);
    }
}

fn log_summary(dir: &Path, passed: u64, failed: u64) {
    let line = format!("Summary: {} passed, {} failed\n", passed, failed);
    let log_path = dir.join("e2e.log");
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    println!();
    let colored = format!(
        "Summary: {} passed, {} failed",
        passed.green(),
        failed.red()
    );
    println!("{}", colored);
}
