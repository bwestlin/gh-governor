use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use owo_colors::{OwoColorize, Stream};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::diff::{RepoSettingsDiff, diff_labels, diff_repo_settings};
use crate::error::Result;
use crate::github::{GithubClient, LabelUsageEntry};
use crate::merge::{MergedRepoConfig, merge_sets_for_repo};
use crate::sets::{IssueTemplateFile, LabelSpec, SetDefinition};
use crate::settings::BranchProtectionRule;

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Plan,
    Apply,
}

const PR_BRANCH_PREFIX: &str = "gh-governor/updates-";

pub async fn run(
    mode: Mode,
    root: crate::config::RootConfig,
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

    handle_repos(mode, &gh, merged, verbose).await
}

async fn handle_repos(
    mode: Mode,
    gh: &GithubClient,
    merged: Vec<(String, MergedRepoConfig)>,
    verbose: bool,
) -> Result<()> {
    for (repo_name, merged_cfg) in merged {
        let repo_info = gh.get_repo(&repo_name).await?;
        let base_branch = repo_info
            .default_branch
            .clone()
            .unwrap_or_else(|| "main".to_string());

        let (settings_diff, desired_settings) = if let Some(desired) = &merged_cfg.repo_settings {
            let current = gh.get_repo_settings(&repo_name).await?;
            (Some(diff_repo_settings(desired, &current)), Some(desired))
        } else {
            (None, None)
        };

        let existing_pr = gh
            .find_open_pr_by_head_prefix(&repo_name, PR_BRANCH_PREFIX, &base_branch)
            .await?;
        let compare_branch = existing_pr.as_ref().map(|pr| pr.head.ref_field.clone());

        let mut bp_changes: Vec<BranchProtectionChange> = Vec::new();
        if let Some(cfg) = desired_settings.and_then(|s| s.branch_protection.as_ref()) {
            for rule in &cfg.rules {
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
        let mut templates_remove: Vec<(String, String)> = Vec::new(); // (path, sha)
        for tpl in &desired_templates {
            match gh
                .get_file(&repo_name, &tpl.path, compare_branch.as_deref())
                .await?
            {
                None => templates_add.push(tpl.clone()),
                Some(file) if file.content != tpl.contents => {
                    templates_update.push((tpl.clone(), file.sha))
                }
                _ => {}
            }
        }
        if let Some(branch_ref) = compare_branch.as_deref() {
            let current_paths = gh
                .list_github_files(&repo_name, branch_ref, ".github/")
                .await
                .unwrap_or_default();
            for path in current_paths {
                if path.starts_with(".github/ISSUE_TEMPLATE/")
                    && !desired_templates
                        .iter()
                        .any(|t| short_github_path(&t.path) == path)
                {
                    if let Some(file) = gh
                        .get_file(&repo_name, &path, compare_branch.as_deref())
                        .await?
                    {
                        templates_remove.push((path, file.sha));
                    }
                }
            }
        }
        let any_file_changes = !templates_add.is_empty()
            || !templates_update.is_empty()
            || !templates_remove.is_empty();

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
                let (pr_note, pr_branch_display) = if any_file_changes {
                    if let Some(pr) = &existing_pr {
                        let branch = pr.head.ref_field.clone();
                        (
                            format!(
                                "draft PR will be updated for .github file updates (reusing #{})",
                                pr.number
                            ),
                            Some(branch),
                        )
                    } else {
                        let branch_name = format!("{PR_BRANCH_PREFIX}{base_branch}");
                        (
                            "draft PR will be created for .github file updates".to_string(),
                            Some(branch_name),
                        )
                    }
                } else if let Some(pr) = &existing_pr {
                    (
                        format!(
                            "existing draft PR #{} already present for .github files",
                            pr.number
                        ),
                        Some(pr.head.ref_field.clone()),
                    )
                } else {
                    ("no PR (no .github file changes)".to_string(), None)
                };
                println!(
                    "Repo {} (plan):\n  Repo settings changes ({}) :{}\n  Branch protection ({}) :{}\n  PR:\n    {}{}\n    .github files add ({}) :{}\n    .github files update ({}) :{}\n    .github files remove ({}) :{}\n  Add labels ({}) :{}\n  Update labels ({}) :{}\n  Remove labels ({}) :{}\n  Blocked removals ({}) :{}",
                    repo_name,
                    settings_count,
                    settings_lines,
                    bp_count,
                    bp_lines,
                    pr_note,
                    pr_branch_display
                        .as_ref()
                        .map(|b| format!(" on branch '{}'\n", b))
                        .unwrap_or_else(String::new),
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
                    format_count(templates_remove.len(), ColorKind::Remove),
                    format_remove_lines(&templates_remove),
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

                let any_file_changes = !templates_add.is_empty() || !templates_update.is_empty();
                let existing_pr = if any_file_changes || existing_pr.is_some() {
                    gh.find_open_pr_by_head_prefix(&repo_name, PR_BRANCH_PREFIX, &base_branch)
                        .await?
                } else {
                    None
                };
                let branch_name = if let Some(pr) = &existing_pr {
                    Some(pr.head.ref_field.clone())
                } else if any_file_changes {
                    let name = format!("{PR_BRANCH_PREFIX}{}", base_branch);
                    let base_sha = gh.get_branch_sha(&repo_name, &base_branch).await?;
                    gh.create_branch_from(&repo_name, &name, &base_sha).await?;
                    Some(name)
                } else {
                    None
                };

                if let Some(branch_ref) = branch_name.as_deref() {
                    for tpl in &templates_add {
                        let msg = format!("Add .github file {} via gh-governor", tpl.path);
                        gh.put_file(
                            &repo_name,
                            &tpl.path,
                            &tpl.contents,
                            None,
                            &msg,
                            Some(branch_ref),
                        )
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
                            Some(branch_ref),
                        )
                        .await?;
                    }
                    for (path, sha) in &templates_remove {
                        let msg = format!("Remove .github file {} via gh-governor", path);
                        gh.delete_file(&repo_name, path, sha, &msg, Some(branch_ref))
                            .await?;
                    }
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

                let mut pr_status = "no PR (no .github file changes)".to_string();
                if let Some(branch) = branch_name.as_deref() {
                    let pr_title =
                        format!("gh-governor updates ({})", Utc::now().format("%Y-%m-%d"));
                    let pr_body = Some("Automated .github updates via gh-governor");
                    let mut pr_opt = existing_pr;
                    if let Some(pr) = pr_opt.as_ref() {
                        if pr.title.as_deref() != Some(&pr_title) || pr.body.as_deref() != pr_body {
                            gh.update_pull_request(&repo_name, pr.number, &pr_title, pr_body)
                                .await?;
                        }
                    }
                    if pr_opt.is_none() && any_file_changes {
                        gh.create_pull_request(
                            &repo_name,
                            &pr_title,
                            branch,
                            &base_branch,
                            pr_body,
                            true,
                        )
                        .await?;
                        pr_opt = gh
                            .find_open_pr_by_head_prefix(&repo_name, PR_BRANCH_PREFIX, &base_branch)
                            .await?;
                    }
                    if let Some(pr) = pr_opt {
                        let url =
                            pr.html_url
                                .as_ref()
                                .map(|u| u.to_string())
                                .unwrap_or_else(|| {
                                    format!(
                                        "https://github.com/{}/{}/pull/{}",
                                        gh.org, repo_name, pr.number
                                    )
                                });
                        pr_status = format!(
                            "draft PR #{} ({} -> {}) [{}]",
                            pr.number, branch, base_branch, url
                        );
                    } else {
                        pr_status = format!(
                            "no PR created for branch '{}' (no changes to apply)",
                            branch
                        );
                    }
                }

                let (settings_count, settings_lines) = format_repo_settings(settings_diff.as_ref());
                let (bp_count, bp_lines) = format_branch_protection(&bp_changes, verbose);
                println!(
                    "Repo {} (apply):\n  Repo settings changes ({}) :{}\n  Branch protection ({}) :{}\n  PR:\n    {}\n    .github files added ({}) :{}\n    .github files updated ({}) :{}\n    .github files removed ({}) :{}\n  Added labels ({}) :{}\n  Updated labels ({}) :{}\n  Removed labels ({}) :{}",
                    repo_name,
                    settings_count,
                    settings_lines,
                    bp_count,
                    bp_lines,
                    pr_status,
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
                    format_count(templates_remove.len(), ColorKind::Remove),
                    format_remove_lines(&templates_remove),
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

fn format_remove_lines(files: &[(String, String)]) -> String {
    if files.is_empty() {
        return " none".to_string();
    }
    let mut out = String::new();
    for (path, _) in files {
        out.push('\n');
        out.push_str(&format!("    - {}", apply_color(path, ColorKind::Remove)));
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
                        .clone()
                        .unwrap_or_else(|| "unset".to_string()),
                    apply_color(&change.desired, ColorKind::Update)
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

fn prepare_merged(
    root: &crate::config::RootConfig,
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
                let loaded = crate::sets::load_set(sets_dir, set_name)?;
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
            return Err(crate::error::Error::MergeConflict {
                repo: repo.name.clone(),
                reason,
            });
        }

        match merge_sets_for_repo(&set_defs) {
            Ok(m) => merged.push((repo.name.clone(), m)),
            Err(err) => {
                return Err(crate::error::Error::MergeConflict {
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
