use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use owo_colors::{OwoColorize, Stream};
use tracing::{error, info};

use gh_governor::config::{RootConfig, load_root_config, resolve_sets_dir};
use gh_governor::diff::diff_labels;
use gh_governor::error::Result;
use gh_governor::github::{GithubClient, LabelUsageEntry};
use gh_governor::merge::{MergedRepoConfig, merge_sets_for_repo};
use gh_governor::sets::{LabelSpec, SetDefinition};

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
    /// Placeholder for applying changes (will create draft PRs)
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

#[derive(Clone, Copy, Debug)]
enum Mode {
    Plan,
    Apply,
}

async fn run(
    mode: Mode,
    root: RootConfig,
    root_path: PathBuf,
    sets_dir: PathBuf,
    only_repos: Vec<String>,
    gh: GithubClient,
    verbose: bool,
) -> Result<()> {
    let merged = prepare_merged(&root, &sets_dir, &only_repos)?;
    info!(
        "loaded config for org '{}' from {}",
        root.org,
        root_path.display()
    );

    match mode {
        Mode::Plan => handle_labels(Mode::Plan, &gh, merged, verbose).await?,
        Mode::Apply => handle_labels(Mode::Apply, &gh, merged, verbose).await?,
    }

    Ok(())
}

async fn handle_labels(
    mode: Mode,
    gh: &GithubClient,
    merged: Vec<(String, MergedRepoConfig)>,
    verbose: bool,
) -> Result<()> {
    for (repo_name, merged_cfg) in merged {
        let current_labels = gh.list_repo_labels(&repo_name).await?;
        let diff = diff_labels(&merged_cfg.labels, &current_labels);

        let mut blocked_removals: Vec<(LabelSpec, Vec<LabelUsageEntry>)> = Vec::new();
        let mut removable = Vec::new();

        for label in &diff.to_remove {
            match gh.label_usage(&repo_name, &label.name, verbose).await? {
                Some(usage) => blocked_removals.push((label.clone(), usage)),
                None => removable.push(label.clone()),
            }
        }

        match mode {
            Mode::Plan => {
                println!(
                    "Repo {} (plan):\n  Add labels ({}) :{}\n  Update labels ({}) :{}\n  Remove labels ({}) :{}\n  Blocked removals ({}) :{}\n  Note: templates/settings apply not yet implemented",
                    repo_name,
                    format_count(diff.to_add.len(), ColorKind::Add),
                    format_label_lines(&diff.to_add, ColorKind::Add),
                    format_count(diff.to_update.len(), ColorKind::Update),
                    format_label_lines(&diff.to_update, ColorKind::Update),
                    format_count(removable.len(), ColorKind::Remove),
                    format_label_lines(&removable, ColorKind::Remove),
                    format_count(blocked_removals.len(), ColorKind::Blocked),
                    format_blocked_lines(&blocked_removals, verbose),
                );
            }
            Mode::Apply => {
                for label in &diff.to_add {
                    gh.create_label(&repo_name, label).await?;
                }
                for label in &diff.to_update {
                    gh.update_label(&repo_name, label).await?;
                }
                for label in &removable {
                    gh.delete_label(&repo_name, &label.name).await?;
                }
                if !blocked_removals.is_empty() {
                    println!(
                        "Repo {} (apply): skipped removal of labels with issues/PRs:{}",
                        repo_name,
                        format_blocked_lines(&blocked_removals, verbose)
                    );
                }
                println!(
                    "Repo {} (apply):\n  Added labels ({}) :{}\n  Updated labels ({}) :{}\n  Removed labels ({}) :{}\n  Note: templates/settings apply not yet implemented",
                    repo_name,
                    format_count(diff.to_add.len(), ColorKind::Add),
                    format_label_lines(&diff.to_add, ColorKind::Add),
                    format_count(diff.to_update.len(), ColorKind::Update),
                    format_label_lines(&diff.to_update, ColorKind::Update),
                    format_count(
                        diff.to_remove.len() - blocked_removals.len(),
                        ColorKind::Remove
                    ),
                    format_label_lines(&removable, ColorKind::Remove),
                );
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ColorKind {
    Add,
    Update,
    Remove,
    Blocked,
}

fn format_count(count: usize, kind: ColorKind) -> String {
    if count == 0 {
        return count.to_string();
    }
    apply_color(&count.to_string(), kind)
}

fn apply_color(text: &str, kind: ColorKind) -> String {
    text.if_supports_color(Stream::Stdout, |t| match kind {
        ColorKind::Add => t.green().bold().to_string(),
        ColorKind::Update => t.cyan().bold().to_string(),
        ColorKind::Remove => t.red().bold().to_string(),
        ColorKind::Blocked => t.yellow().bold().to_string(),
    })
    .to_string()
}

fn format_label_lines(labels: &[LabelSpec], kind: ColorKind) -> String {
    if labels.is_empty() {
        return " none".to_string();
    }
    let mut out = String::new();
    for label in labels {
        let mut line = format!("    - {}", apply_color(&label.name, kind));
        if let Some(color) = &label.color {
            line.push_str(&format!(" (#{})", color));
        }
        if let Some(desc) = &label.description {
            line.push_str(&format!(" \"{}\"", desc));
        }
        out.push('\n');
        out.push_str(&line);
    }
    out
}

fn format_blocked_lines(blocked: &[(LabelSpec, Vec<LabelUsageEntry>)], verbose: bool) -> String {
    if blocked.is_empty() {
        return " none".to_string();
    }
    let mut out = String::new();
    for (label, usage) in blocked {
        let mut line = format!("    - {}", apply_color(&label.name, ColorKind::Blocked));
        if let Some(color) = &label.color {
            line.push_str(&format!(" (#{})", color));
        }
        if let Some(desc) = &label.description {
            line.push_str(&format!(" \"{}\"", desc));
        }
        if verbose && !usage.is_empty() {
            line.push_str(" -> in use by:");
            out.push('\n');
            out.push_str(&line);
            for u in usage {
                let kind = if u.is_pr { "PR" } else { "Issue" };
                let entry = match (&u.url, u.number) {
                    (Some(url), n) if n > 0 => format!("{} {} ({})", kind, n, url),
                    (Some(url), _) => format!("{} ({})", kind, url),
                    (None, n) if n > 0 => format!("{} {}", kind, n),
                    _ => kind.to_string(),
                };
                out.push('\n');
                out.push_str(&format!("      - {}", entry));
            }
            continue;
        }
        out.push('\n');
        out.push_str(&line);
    }
    out
}

fn prepare_merged(
    root: &RootConfig,
    sets_dir: &PathBuf,
    only_repos: &[String],
) -> Result<Vec<(String, MergedRepoConfig)>> {
    let mut set_cache: HashMap<String, SetDefinition> = HashMap::new();
    let mut merged = Vec::new();

    for repo in root.repos.iter() {
        if !only_repos.is_empty() && !only_repos.contains(&repo.name) {
            continue;
        }

        let mut set_defs = Vec::new();
        for set_name in root.default_sets.iter().chain(repo.sets.iter()) {
            if !set_cache.contains_key(set_name) {
                let loaded = gh_governor::sets::load_set(&sets_dir, set_name)?;
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
            Ok(m) => merged.push((repo.name.clone(), m)),
            Err(err) => {
                error!("repo '{}': merge failed: {err}", repo.name);
            }
        }
    }

    Ok(merged)
}
