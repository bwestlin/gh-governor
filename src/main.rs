use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use owo_colors::{OwoColorize, Stream};
use serde::{Deserialize, Serialize};
use tracing::info;

use gh_governor::config::{RootConfig, load_root_config, resolve_sets_dir};
use gh_governor::diff::{RepoSettingsDiff, diff_labels, diff_repo_settings};
use gh_governor::error::Result;
use gh_governor::github::{GithubClient, LabelUsageEntry};
use gh_governor::merge::{MergedRepoConfig, merge_sets_for_repo};
use gh_governor::sets::{IssueTemplateFile, LabelSpec, SetDefinition};
use gh_governor::settings::BranchProtectionRule;

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
        Mode::Plan => handle_repos(Mode::Plan, &gh, merged, verbose).await?,
        Mode::Apply => handle_repos(Mode::Apply, &gh, merged, verbose).await?,
    }

    Ok(())
}

async fn handle_repos(
    mode: Mode,
    gh: &GithubClient,
    merged: Vec<(String, MergedRepoConfig)>,
    verbose: bool,
) -> Result<()> {
    for (repo_name, merged_cfg) in merged {
        let (settings_diff, desired_settings) = if let Some(desired) = &merged_cfg.repo_settings {
            let current = gh.get_repo_settings(&repo_name).await?;
            (Some(diff_repo_settings(desired, &current)), Some(desired))
        } else {
            (None, None)
        };

        let mut bp_changes: Vec<BranchProtectionChange> = Vec::new();
        if let Some(bp_cfg) = &merged_cfg.branch_protection {
            for rule in &bp_cfg.rules {
                let current = gh.get_branch_protection(&repo_name, &rule.pattern).await?;
                let target = merge_branch_rule(rule, current.as_ref());
                if current.as_ref() != Some(&target) {
                    bp_changes.push(BranchProtectionChange {
                        pattern: rule.pattern.clone(),
                        action: if current.is_some() {
                            ChangeAction::Update
                        } else {
                            ChangeAction::Create
                        },
                        target,
                    });
                }
            }
        }

        let mut desired_templates: Vec<IssueTemplateFile> = merged_cfg
            .issue_templates
            .iter()
            .filter(|t| !short_github_path(&t.path).ends_with("config.yml"))
            .cloned()
            .collect();

        if let Some(cfg) = build_issue_template_config(&merged_cfg.issue_templates) {
            desired_templates.push(cfg);
        }

        let mut templates_add: Vec<IssueTemplateFile> = Vec::new();
        let mut templates_update: Vec<(IssueTemplateFile, String)> = Vec::new();
        for tpl in &desired_templates {
            match gh.get_file(&repo_name, &tpl.path).await? {
                None => templates_add.push(tpl.clone()),
                Some(file) if file.content != tpl.contents => {
                    templates_update.push((tpl.clone(), file.sha))
                }
                _ => {}
            }
        }

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
                let (settings_count, settings_lines) = format_repo_settings(settings_diff.as_ref());
                let (bp_count, bp_lines) = format_branch_protection(&bp_changes, verbose);
                println!(
                    "Repo {} (plan):\n  Repo settings changes ({}) :{}\n  Branch protection ({}) :{}\n  .github files add ({}) :{}\n  .github files update ({}) :{}\n  Add labels ({}) :{}\n  Update labels ({}) :{}\n  Remove labels ({}) :{}\n  Blocked removals ({}) :{}",
                    repo_name,
                    settings_count,
                    settings_lines,
                    bp_count,
                    bp_lines,
                    format_count(templates_add.len(), ColorKind::Add),
                    format_template_lines(&templates_add, ColorKind::Add),
                    format_count(templates_update.len(), ColorKind::Update),
                    format_template_lines(
                        &templates_update
                            .iter()
                            .map(|(t, _)| t.clone())
                            .collect::<Vec<_>>(),
                        ColorKind::Update
                    ),
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
                if let (Some(diff_settings), Some(desired)) = (&settings_diff, desired_settings) {
                    if !diff_settings.changes.is_empty() {
                        gh.update_repo_settings(&repo_name, desired).await?;
                    }
                }

                for bp in &bp_changes {
                    gh.set_branch_protection(&repo_name, &bp.target).await?;
                }

                for tpl in &templates_add {
                    let msg = format!("Add .github file {} via gh-governor", tpl.path);
                    gh.put_file(&repo_name, &tpl.path, &tpl.contents, None, &msg)
                        .await?;
                }
                for (tpl, sha) in &templates_update {
                    let msg = format!("Update .github file {} via gh-governor", tpl.path);
                    gh.put_file(
                        &repo_name,
                        &tpl.path,
                        &tpl.contents,
                        Some(sha.clone()),
                        &msg,
                    )
                    .await?;
                }

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
                let (settings_count, settings_lines) = format_repo_settings(settings_diff.as_ref());
                let (bp_count, bp_lines) = format_branch_protection(&bp_changes, verbose);
                println!(
                    "Repo {} (apply):\n  Repo settings changes ({}) :{}\n  Branch protection ({}) :{}\n  .github files added ({}) :{}\n  .github files updated ({}) :{}\n  Added labels ({}) :{}\n  Updated labels ({}) :{}\n  Removed labels ({}) :{}",
                    repo_name,
                    settings_count,
                    settings_lines,
                    bp_count,
                    bp_lines,
                    format_count(templates_add.len(), ColorKind::Add),
                    format_template_lines(&templates_add, ColorKind::Add),
                    format_count(templates_update.len(), ColorKind::Update),
                    format_template_lines(
                        &templates_update
                            .iter()
                            .map(|(t, _)| t.clone())
                            .collect::<Vec<_>>(),
                        ColorKind::Update
                    ),
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

#[derive(Clone)]
struct BranchProtectionChange {
    pattern: String,
    action: ChangeAction,
    target: BranchProtectionRule,
}

#[derive(Clone, Copy)]
enum ChangeAction {
    Create,
    Update,
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
        ColorKind::Remove => t.magenta().bold().to_string(),
        ColorKind::Blocked => t.red().bold().to_string(),
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

fn format_template_lines(templates: &[IssueTemplateFile], kind: ColorKind) -> String {
    if templates.is_empty() {
        return " none".to_string();
    }
    let mut out = String::new();
    for tpl in templates {
        out.push('\n');
        out.push_str(&format!(
            "    - {}",
            apply_color(&short_github_path(&tpl.path), kind)
        ));
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

fn format_repo_settings(diff: Option<&RepoSettingsDiff>) -> (String, String) {
    match diff {
        None => ("not configured".to_string(), " not configured".to_string()),
        Some(d) if d.changes.is_empty() => ("0".to_string(), " none".to_string()),
        Some(d) => {
            let mut out = String::new();
            for change in &d.changes {
                let line = format!(
                    "    - {}: {} -> {}",
                    change.field,
                    change
                        .current
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unset".to_string()),
                    apply_color(&change.desired.to_string(), ColorKind::Update)
                );
                out.push('\n');
                out.push_str(&line);
            }
            (format_count(d.changes.len(), ColorKind::Update), out)
        }
    }
}

fn format_branch_protection(changes: &[BranchProtectionChange], verbose: bool) -> (String, String) {
    if changes.is_empty() {
        return ("0".to_string(), " none".to_string());
    }
    let mut out = String::new();
    for change in changes {
        let action = match change.action {
            ChangeAction::Create => "create",
            ChangeAction::Update => "update",
        };
        out.push('\n');
        out.push_str(&format!(
            "    - {}: {}",
            apply_color(&change.pattern, ColorKind::Update),
            action
        ));
        if verbose {
            for detail in branch_rule_details(&change.target) {
                out.push('\n');
                out.push_str(&format!("      - {}", detail));
            }
        }
    }
    (format_count(changes.len(), ColorKind::Update), out)
}

fn merge_branch_rule(
    desired: &BranchProtectionRule,
    current: Option<&BranchProtectionRule>,
) -> BranchProtectionRule {
    let mut merged = desired.clone();
    if let Some(cur) = current {
        if merged.required_status_checks.is_none() {
            merged.required_status_checks = cur.required_status_checks.clone();
        }
        if merged.required_pull_request_reviews.is_none() {
            merged.required_pull_request_reviews = cur.required_pull_request_reviews.clone();
        }
        if merged.enforce_admins.is_none() {
            merged.enforce_admins = cur.enforce_admins;
        }
        if merged.restrictions.is_none() {
            merged.restrictions = cur.restrictions.clone();
        }
        if merged.allow_force_pushes.is_none() {
            merged.allow_force_pushes = cur.allow_force_pushes;
        }
        if merged.allow_deletions.is_none() {
            merged.allow_deletions = cur.allow_deletions;
        }
        if merged.block_creations.is_none() {
            merged.block_creations = cur.block_creations;
        }
        if merged.require_linear_history.is_none() {
            merged.require_linear_history = cur.require_linear_history;
        }
        if merged.required_conversation_resolution.is_none() {
            merged.required_conversation_resolution = cur.required_conversation_resolution;
        }
        if merged.required_signatures.is_none() {
            merged.required_signatures = cur.required_signatures;
        }
    }
    merged
}

fn branch_rule_details(rule: &BranchProtectionRule) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(sc) = &rule.required_status_checks {
        if let Some(strict) = sc.strict {
            lines.push(format!("status checks strict: {}", strict));
        }
        if let Some(ctx) = &sc.contexts {
            if !ctx.is_empty() {
                lines.push(format!("status contexts: {}", ctx.join(", ")));
            }
        }
        if let Some(checks) = &sc.checks {
            if !checks.is_empty() {
                let list: Vec<String> = checks
                    .iter()
                    .map(|c| {
                        if let Some(app) = c.app_id {
                            format!("{} (app {})", c.context, app)
                        } else {
                            c.context.clone()
                        }
                    })
                    .collect();
                lines.push(format!("status checks: {}", list.join(", ")));
            }
        }
    }
    if let Some(pr) = &rule.required_pull_request_reviews {
        if let Some(v) = pr.dismiss_stale_reviews {
            lines.push(format!("dismiss stale reviews: {}", v));
        }
        if let Some(v) = pr.require_code_owner_reviews {
            lines.push(format!("require code owner reviews: {}", v));
        }
        if let Some(v) = pr.required_approving_review_count {
            lines.push(format!("required approvals: {}", v));
        }
        if let Some(v) = pr.require_last_push_approval {
            lines.push(format!("require last push approval: {}", v));
        }
        if let Some(d) = &pr.dismissal_restrictions {
            let users = d.users.as_ref().map(|u| u.join(", ")).unwrap_or_default();
            let teams = d.teams.as_ref().map(|t| t.join(", ")).unwrap_or_default();
            let mut parts = Vec::new();
            if !users.is_empty() {
                parts.push(format!("users [{}]", users));
            }
            if !teams.is_empty() {
                parts.push(format!("teams [{}]", teams));
            }
            if !parts.is_empty() {
                lines.push(format!("dismissal restrictions: {}", parts.join("; ")));
            }
        }
    }
    if let Some(v) = rule.enforce_admins {
        lines.push(format!("enforce admins: {}", v));
    }
    if let Some(v) = rule.allow_force_pushes {
        lines.push(format!("allow force pushes: {}", v));
    }
    if let Some(v) = rule.allow_deletions {
        lines.push(format!("allow deletions: {}", v));
    }
    if let Some(v) = rule.block_creations {
        lines.push(format!("block creations: {}", v));
    }
    if let Some(v) = rule.require_linear_history {
        lines.push(format!("require linear history: {}", v));
    }
    if let Some(v) = rule.required_conversation_resolution {
        lines.push(format!("require conversation resolution: {}", v));
    }
    if let Some(v) = rule.required_signatures {
        lines.push(format!("require signatures: {}", v));
    }
    if let Some(r) = &rule.restrictions {
        let users = r.users.as_ref().map(|u| u.join(", ")).unwrap_or_default();
        let teams = r.teams.as_ref().map(|t| t.join(", ")).unwrap_or_default();
        let apps = r.apps.as_ref().map(|a| a.join(", ")).unwrap_or_default();
        let mut parts = Vec::new();
        if !users.is_empty() {
            parts.push(format!("users [{}]", users));
        }
        if !teams.is_empty() {
            parts.push(format!("teams [{}]", teams));
        }
        if !apps.is_empty() {
            parts.push(format!("apps [{}]", apps));
        }
        if !parts.is_empty() {
            lines.push(format!("restrictions: {}", parts.join("; ")));
        }
    }
    lines
}

fn short_github_path(path: &str) -> String {
    if let Some(idx) = path.find(".github/") {
        path[idx..].to_string()
    } else {
        path.to_string()
    }
}

fn build_issue_template_config(templates: &[IssueTemplateFile]) -> Option<IssueTemplateFile> {
    let base = templates
        .iter()
        .find(|t| short_github_path(&t.path).ends_with("config.yml"));

    let desired_templates: Vec<IssueTemplateEntry> = templates
        .iter()
        .filter(|t| !short_github_path(&t.path).ends_with("config.yml"))
        .map(|tpl| IssueTemplateEntry {
            name: parse_template_name(&tpl.contents)
                .or_else(|| file_stem(&tpl.path).map(|s| s.to_string())),
            description: parse_template_description(&tpl.contents),
            file: file_name(&tpl.path).unwrap_or_else(|| tpl.path.clone()),
        })
        .collect();

    if desired_templates.is_empty() && base.is_none() {
        return None;
    }

    let mut config = base
        .and_then(|c| serde_yaml::from_str::<IssueTemplateConfig>(&c.contents).ok())
        .unwrap_or_default();
    config.issue_templates = Some(desired_templates);

    let contents = serde_yaml::to_string(&config).unwrap_or_default();
    Some(IssueTemplateFile {
        path: ".github/ISSUE_TEMPLATE/config.yml".to_string(),
        contents,
    })
}

fn parse_template_name(contents: &str) -> Option<String> {
    serde_yaml::from_str::<TemplateFrontMatter>(contents)
        .ok()
        .and_then(|f| f.name)
}

fn parse_template_description(contents: &str) -> Option<String> {
    serde_yaml::from_str::<TemplateFrontMatter>(contents)
        .ok()
        .and_then(|f| f.description)
}

fn file_name(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

fn file_stem(path: &str) -> Option<String> {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IssueTemplateConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    blank_issues_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contact_links: Option<Vec<ContactLink>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_templates: Option<Vec<IssueTemplateEntry>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ContactLink {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    about: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IssueTemplateEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    file: String,
}

#[derive(Debug, Deserialize)]
struct TemplateFrontMatter {
    name: Option<String>,
    description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_with_tpl(name: &str, path: &str, contents: &str) -> SetDefinition {
        SetDefinition {
            name: name.to_string(),
            path: PathBuf::new(),
            labels: Vec::new(),
            issue_templates: vec![IssueTemplateFile {
                path: path.to_string(),
                contents: contents.to_string(),
            }],
            repo_settings: None,
            branch_protection: None,
            checks: None,
        }
    }

    #[test]
    fn detects_template_conflict_between_sets() {
        let a = set_with_tpl("a", ".github/ISSUE_TEMPLATE/bug.yml", "one");
        let b = set_with_tpl("b", ".github/ISSUE_TEMPLATE/bug.yml", "two");
        let err = detect_template_conflicts(&[a, b]).unwrap_err();
        assert!(err.contains(
            "conflicting .github file '.github/ISSUE_TEMPLATE/bug.yml' between sets 'a' and 'b'"
        ));
    }

    #[test]
    fn allows_identical_templates_across_sets() {
        let a = set_with_tpl("a", ".github/ISSUE_TEMPLATE/bug.yml", "same");
        let b = set_with_tpl("b", ".github/ISSUE_TEMPLATE/bug.yml", "same");
        assert!(detect_template_conflicts(&[a, b]).is_ok());
    }

    #[test]
    fn builds_config_including_templates() {
        let templates = vec![
            IssueTemplateFile {
                path: "example-conf/toml/config-sets/core/.github/ISSUE_TEMPLATE/bug.yml"
                    .to_string(),
                contents: "name: Bug\ndescription: A bug\n".to_string(),
            },
            IssueTemplateFile {
                path: ".github/ISSUE_TEMPLATE/feature.yml".to_string(),
                contents: "name: Feature\n".to_string(),
            },
        ];
        let cfg = build_issue_template_config(&templates).expect("config");
        assert_eq!(cfg.path, ".github/ISSUE_TEMPLATE/config.yml");
        assert!(cfg.contents.contains("bug.yml"));
        assert!(cfg.contents.contains("feature.yml"));
    }
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

        if let Err(reason) = detect_template_conflicts(&set_defs) {
            return Err(gh_governor::error::Error::MergeConflict {
                repo: repo.name.clone(),
                reason,
            });
        }

        match merge_sets_for_repo(&set_defs) {
            Ok(m) => merged.push((repo.name.clone(), m)),
            Err(err) => {
                return Err(gh_governor::error::Error::MergeConflict {
                    repo: repo.name.clone(),
                    reason: err.to_string(),
                });
            }
        }
    }

    Ok(merged)
}

fn detect_template_conflicts(sets: &[SetDefinition]) -> std::result::Result<(), String> {
    let mut seen: HashMap<String, (String, String)> = HashMap::new(); // normalized path -> (contents, set name)
    for set in sets {
        for tpl in &set.issue_templates {
            let key = short_github_path(&tpl.path);
            if let Some(existing) = seen.get(&key) {
                if existing.0 != tpl.contents {
                    return Err(format!(
                        "conflicting .github file '{}' between sets '{}' and '{}'",
                        key, existing.1, set.name
                    ));
                }
            } else {
                seen.insert(key, (tpl.contents.clone(), set.name.clone()));
            }
        }
    }
    Ok(())
}
