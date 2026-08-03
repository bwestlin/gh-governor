use std::collections::HashMap;

use thiserror::Error;

use crate::sets::ChecksConfig;
use crate::sets::GithubFile;
use crate::sets::LabelSpec;
use crate::sets::SetDefinition;
use crate::settings::BranchProtectionRule;
use crate::settings::BranchRestrictions;
use crate::settings::PullRequestSettings;
use crate::settings::RepoSettings;
use crate::settings::RequiredPullRequestReviews;
use crate::settings::RequiredStatusChecks;
use crate::settings::ReviewDismissalRestrictions;

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("label conflict for '{0}' between sets; definitions differ")]
    LabelConflict(String),
    #[error(".github file conflict for '{0}' between sets")]
    TemplateConflict(String),
    #[error("{0}")]
    GenericConflict(String),
}

pub type MergeResult<T> = Result<T, MergeError>;

fn format_set_names(names: &[String]) -> String {
    match names {
        [name] => format!("set '{}'", name),
        _ => format!(
            "sets {}",
            names
                .iter()
                .map(|name| format!("'{}'", name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn merge_pull_request_field<T: std::fmt::Debug + PartialEq>(
    current: Option<T>,
    incoming: Option<T>,
    field: &str,
    conflicts: &mut Vec<String>,
) -> Option<T> {
    match (current, incoming) {
        (None, value) => value,
        (value, None) => value,
        (Some(current), Some(incoming)) if current == incoming => Some(current),
        (Some(current), Some(incoming)) => {
            conflicts.push(format!("{}: {:?} vs {:?}", field, current, incoming));
            Some(current)
        }
    }
}

fn merge_pull_request_settings(
    current: PullRequestSettings,
    incoming: PullRequestSettings,
) -> Result<PullRequestSettings, String> {
    let mut conflicts = Vec::new();
    let merged = PullRequestSettings {
        allow_merge_commit: merge_pull_request_field(
            current.allow_merge_commit,
            incoming.allow_merge_commit,
            "allow_merge_commit",
            &mut conflicts,
        ),
        allow_squash_merge: merge_pull_request_field(
            current.allow_squash_merge,
            incoming.allow_squash_merge,
            "allow_squash_merge",
            &mut conflicts,
        ),
        allow_rebase_merge: merge_pull_request_field(
            current.allow_rebase_merge,
            incoming.allow_rebase_merge,
            "allow_rebase_merge",
            &mut conflicts,
        ),
        allow_auto_merge: merge_pull_request_field(
            current.allow_auto_merge,
            incoming.allow_auto_merge,
            "allow_auto_merge",
            &mut conflicts,
        ),
        delete_branch_on_merge: merge_pull_request_field(
            current.delete_branch_on_merge,
            incoming.delete_branch_on_merge,
            "delete_branch_on_merge",
            &mut conflicts,
        ),
        merge_commit_message_option: merge_pull_request_field(
            current.merge_commit_message_option,
            incoming.merge_commit_message_option,
            "merge_commit_message_option",
            &mut conflicts,
        ),
        squash_merge_option: merge_pull_request_field(
            current.squash_merge_option,
            incoming.squash_merge_option,
            "squash_merge_option",
            &mut conflicts,
        ),
    };

    if conflicts.is_empty() {
        Ok(merged)
    } else {
        Err(conflicts.join("\n      - "))
    }
}

fn checks_conflict_details(existing: &ChecksConfig, incoming: &ChecksConfig) -> String {
    let mut differences = Vec::new();
    if existing.require_codeowners != incoming.require_codeowners {
        differences.push(format!(
            "require_codeowners: {} vs {}",
            existing.require_codeowners, incoming.require_codeowners
        ));
    }
    if existing.warn_on_inactive_owners != incoming.warn_on_inactive_owners {
        differences.push(format!(
            "warn_on_inactive_owners: {} vs {}",
            existing.warn_on_inactive_owners, incoming.warn_on_inactive_owners
        ));
    }
    differences.join("\n      - ")
}

#[derive(Debug, Clone)]
pub struct MergedRepoConfig {
    pub labels: Vec<LabelSpec>,
    pub github_files: Vec<GithubFile>,
    pub repo_settings: Option<RepoSettings>,
    pub checks: Option<ChecksConfig>,
}

pub fn merge_sets_for_repo(sets: &[SetDefinition]) -> MergeResult<MergedRepoConfig> {
    let mut labels = HashMap::new();
    let mut github_files = HashMap::new();
    let mut repo_settings: Option<RepoSettings> = None;
    let mut repo_settings_sets = Vec::new();
    let mut pull_request_sets = Vec::new();
    let mut checks: Option<(ChecksConfig, String)> = None;

    for set in sets {
        for label in &set.labels {
            match labels.get(&label.name) {
                Some(existing) if existing != label => {
                    return Err(MergeError::LabelConflict(label.name.clone()));
                }
                _ => {
                    labels.insert(label.name.clone(), label.clone());
                }
            }
        }

        for file in &set.github_files {
            match github_files.get(&file.path) {
                Some(existing) if existing != file => {
                    return Err(MergeError::TemplateConflict(file.path.clone()));
                }
                Some(_) => {}
                None => {
                    github_files.insert(file.path.clone(), file.clone());
                }
            }
        }

        if let Some(settings) = &set.repo_settings {
            repo_settings = merge_repo_settings(
                repo_settings,
                settings.clone(),
                &repo_settings_sets,
                &mut pull_request_sets,
                &set.name,
            )?;
            repo_settings_sets.push(set.name.clone());
        }

        if let Some(chk) = &set.checks {
            match &checks {
                Some((current, existing_set)) if current != chk => {
                    return Err(MergeError::GenericConflict(format!(
                        "checks:\n    conflicting sets: set '{}' and set '{}'\n    differing values:\n      - {}",
                        existing_set,
                        set.name,
                        checks_conflict_details(current, chk)
                    )));
                }
                Some(_) => {}
                None => checks = Some((chk.clone(), set.name.clone())),
            }
        }
    }

    Ok(MergedRepoConfig {
        labels: {
            let mut v: Vec<_> = labels.into_values().collect();
            v.sort_by(|a, b| a.name.cmp(&b.name));
            v
        },
        github_files: {
            let mut v: Vec<_> = github_files.into_values().collect();
            v.sort_by(|a, b| a.path.cmp(&b.path));
            v
        },
        repo_settings,
        checks: checks.map(|(checks, _)| checks),
    })
}

fn merge_repo_settings(
    existing: Option<RepoSettings>,
    incoming: RepoSettings,
    existing_sets: &[String],
    pull_request_sets: &mut Vec<String>,
    incoming_set: &str,
) -> MergeResult<Option<RepoSettings>> {
    match existing {
        None => {
            if incoming.pull_requests.is_some() {
                pull_request_sets.push(incoming_set.to_string());
            }
            Ok(Some(incoming))
        }
        Some(mut current) => {
            // Merge pull request settings field-by-field; unset fields don't conflict.
            match (&mut current.pull_requests, incoming.pull_requests) {
                (slot @ None, Some(pr)) => {
                    *slot = Some(pr);
                    pull_request_sets.push(incoming_set.to_string());
                }
                (Some(_), None) => {}
                (Some(current), Some(incoming)) => {
                    *current = merge_pull_request_settings(current.clone(), incoming).map_err(
                        |details| {
                            MergeError::GenericConflict(format!(
                                "repo settings (pull requests):\n    conflicting sets: {} and set '{}'\n    differing values:\n      - {}",
                                format_set_names(pull_request_sets),
                                incoming_set,
                                details
                            ))
                        },
                    )?;
                    pull_request_sets.push(incoming_set.to_string());
                }
                (None, None) => {}
            }

            // Merge branch protection rules by pattern; merge non-overlapping fields.
            match (&mut current.branch_protection, incoming.branch_protection) {
                (None, Some(bp)) => current.branch_protection = Some(bp),
                (Some(_), None) => {}
                (Some(cur), Some(inc)) => {
                    for rule in inc.rules {
                        match cur.rules.iter_mut().find(|r| r.pattern == rule.pattern) {
                            Some(existing_rule) => {
                                *existing_rule =
                                    merge_branch_rule(existing_rule, rule).map_err(|e| {
                                        MergeError::GenericConflict(format!(
                                            "repo settings (branch protection):\n    conflicting sets: {} and set '{}'\n    rule: '{}'\n    conflict: {}",
                                            format_set_names(existing_sets),
                                            incoming_set,
                                            existing_rule.pattern,
                                            e
                                        ))
                                    })?;
                            }
                            None => cur.rules.push(rule),
                        }
                    }
                }
                _ => {}
            }

            Ok(Some(current))
        }
    }
}

fn merge_branch_rule(
    current: &BranchProtectionRule,
    incoming: BranchProtectionRule,
) -> Result<BranchProtectionRule, String> {
    if current.pattern != incoming.pattern {
        return Err("pattern mismatch".to_string());
    }
    let mut merged = current.clone();

    merged.required_status_checks = merge_optional(
        merged.required_status_checks,
        incoming.required_status_checks,
        merge_required_status_checks,
        "required_status_checks",
    )?;
    merged.required_pull_request_reviews = merge_optional(
        merged.required_pull_request_reviews,
        incoming.required_pull_request_reviews,
        merge_required_pull_request_reviews,
        "required_pull_request_reviews",
    )?;
    merged.enforce_admins = merge_option_field(
        merged.enforce_admins,
        incoming.enforce_admins,
        "enforce_admins",
    )?;
    merged.restrictions = merge_optional(
        merged.restrictions,
        incoming.restrictions,
        merge_branch_restrictions,
        "restrictions",
    )?;
    merged.allow_force_pushes = merge_option_field(
        merged.allow_force_pushes,
        incoming.allow_force_pushes,
        "allow_force_pushes",
    )?;
    merged.allow_deletions = merge_option_field(
        merged.allow_deletions,
        incoming.allow_deletions,
        "allow_deletions",
    )?;
    merged.block_creations = merge_option_field(
        merged.block_creations,
        incoming.block_creations,
        "block_creations",
    )?;
    merged.require_linear_history = merge_option_field(
        merged.require_linear_history,
        incoming.require_linear_history,
        "require_linear_history",
    )?;
    merged.required_conversation_resolution = merge_option_field(
        merged.required_conversation_resolution,
        incoming.required_conversation_resolution,
        "required_conversation_resolution",
    )?;
    merged.required_signatures = merge_option_field(
        merged.required_signatures,
        incoming.required_signatures,
        "required_signatures",
    )?;

    Ok(merged)
}

fn merge_required_status_checks(
    current: RequiredStatusChecks,
    incoming: RequiredStatusChecks,
) -> Result<RequiredStatusChecks, String> {
    Ok(RequiredStatusChecks {
        strict: merge_option_field(current.strict, incoming.strict, "strict")?,
        contexts: merge_option_vec(current.contexts, incoming.contexts),
        checks: merge_option_vec(current.checks, incoming.checks),
    })
}

fn merge_required_pull_request_reviews(
    current: RequiredPullRequestReviews,
    incoming: RequiredPullRequestReviews,
) -> Result<RequiredPullRequestReviews, String> {
    Ok(RequiredPullRequestReviews {
        dismiss_stale_reviews: merge_option_field(
            current.dismiss_stale_reviews,
            incoming.dismiss_stale_reviews,
            "dismiss_stale_reviews",
        )?,
        require_code_owner_reviews: merge_option_field(
            current.require_code_owner_reviews,
            incoming.require_code_owner_reviews,
            "require_code_owner_reviews",
        )?,
        required_approving_review_count: merge_option_field(
            current.required_approving_review_count,
            incoming.required_approving_review_count,
            "required_approving_review_count",
        )?,
        require_last_push_approval: merge_option_field(
            current.require_last_push_approval,
            incoming.require_last_push_approval,
            "require_last_push_approval",
        )?,
        dismissal_restrictions: merge_optional(
            current.dismissal_restrictions,
            incoming.dismissal_restrictions,
            merge_review_dismissal_restrictions,
            "dismissal_restrictions",
        )?,
    })
}

fn merge_review_dismissal_restrictions(
    current: ReviewDismissalRestrictions,
    incoming: ReviewDismissalRestrictions,
) -> Result<ReviewDismissalRestrictions, String> {
    Ok(ReviewDismissalRestrictions {
        users: merge_option_vec(current.users, incoming.users),
        teams: merge_option_vec(current.teams, incoming.teams),
    })
}

fn merge_branch_restrictions(
    current: BranchRestrictions,
    incoming: BranchRestrictions,
) -> Result<BranchRestrictions, String> {
    Ok(BranchRestrictions {
        users: merge_option_vec(current.users, incoming.users),
        teams: merge_option_vec(current.teams, incoming.teams),
        apps: merge_option_vec(current.apps, incoming.apps),
    })
}

fn merge_option_field<T: std::fmt::Debug + PartialEq + Copy>(
    current: Option<T>,
    incoming: Option<T>,
    field: &str,
) -> Result<Option<T>, String> {
    match (current, incoming) {
        (None, v) => Ok(v),
        (v, None) => Ok(v),
        (Some(a), Some(b)) if a == b => Ok(Some(a)),
        (Some(a), Some(b)) => Err(format!(
            "{} is {:?} in the previously merged configuration vs {:?} in the incoming set",
            field, a, b
        )),
    }
}

fn merge_option_vec<T: PartialEq>(
    current: Option<Vec<T>>,
    incoming: Option<Vec<T>>,
) -> Option<Vec<T>> {
    match (current, incoming) {
        (None, None) => None,
        (current, incoming) => {
            let mut merged = Vec::new();
            for item in current
                .into_iter()
                .flatten()
                .chain(incoming.into_iter().flatten())
            {
                if !merged.contains(&item) {
                    merged.push(item);
                }
            }
            Some(merged)
        }
    }
}

fn merge_optional<T, F>(
    current: Option<T>,
    incoming: Option<T>,
    merge: F,
    field: &str,
) -> Result<Option<T>, String>
where
    F: Fn(T, T) -> Result<T, String>,
{
    match (current, incoming) {
        (None, v) => Ok(v),
        (v, None) => Ok(v),
        (Some(a), Some(b)) => merge(a, b)
            .map(Some)
            .map_err(|e| format!("{}: {}", field, e)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sets::{GithubFile, SetDefinition};

    fn base_set(name: &str) -> SetDefinition {
        SetDefinition {
            name: name.to_string(),
            path: "".into(),
            labels: Vec::new(),
            github_files: Vec::new(),
            repo_settings: None,
            checks: None,
        }
    }

    fn set_with_repo_settings(name: &str, config: &str) -> SetDefinition {
        let mut set = base_set(name);
        set.repo_settings = Some(toml::from_str(config).unwrap());
        set
    }

    #[test]
    fn merges_labels_without_conflict() {
        let mut a = base_set("a");
        a.labels.push(LabelSpec {
            name: "bug".to_string(),
            color: Some("ff0000".to_string()),
            description: Some("A bug".to_string()),
        });
        let mut b = base_set("b");
        b.labels.push(LabelSpec {
            name: "feature".to_string(),
            color: None,
            description: None,
        });
        let merged = merge_sets_for_repo(&[a, b]).unwrap();
        assert_eq!(merged.labels.len(), 2);
    }

    #[test]
    fn detects_label_conflict() {
        let mut a = base_set("a");
        a.labels.push(LabelSpec {
            name: "bug".to_string(),
            color: Some("ff0000".to_string()),
            description: None,
        });
        let mut b = base_set("b");
        b.labels.push(LabelSpec {
            name: "bug".to_string(),
            color: Some("00ff00".to_string()),
            description: None,
        });
        assert!(matches!(
            merge_sets_for_repo(&[a, b]),
            Err(MergeError::LabelConflict(_))
        ));
    }

    #[test]
    fn detects_template_conflict() {
        let mut a = base_set("a");
        a.github_files.push(GithubFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: "a".to_string(),
        });
        let mut b = base_set("b");
        b.github_files.push(GithubFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: "b".to_string(),
        });
        assert!(matches!(
            merge_sets_for_repo(&[a, b]),
            Err(MergeError::TemplateConflict(_))
        ));
    }

    #[test]
    fn allows_identical_templates() {
        let mut a = base_set("a");
        a.github_files.push(GithubFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: "same".to_string(),
        });
        let mut b = base_set("b");
        b.github_files.push(GithubFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: "same".to_string(),
        });
        let merged = merge_sets_for_repo(&[a, b]).unwrap();
        assert_eq!(merged.github_files.len(), 1);
    }

    #[test]
    fn merges_pull_request_fields_when_other_set_leaves_them_unset() {
        let core = set_with_repo_settings(
            "core",
            r#"
[pull_requests]
delete_branch_on_merge = true
"#,
        );
        let infra = set_with_repo_settings(
            "infra",
            r#"
[pull_requests]
allow_squash_merge = false
allow_rebase_merge = true
"#,
        );

        let merged = merge_sets_for_repo(&[core, infra]).unwrap();
        let pull_requests = merged.repo_settings.unwrap().pull_requests.unwrap();

        assert_eq!(pull_requests.allow_squash_merge, Some(false));
        assert_eq!(pull_requests.allow_rebase_merge, Some(true));
        assert_eq!(pull_requests.delete_branch_on_merge, Some(true));
    }

    #[test]
    fn pull_request_conflict_names_sets_fields_and_values() {
        let core = set_with_repo_settings(
            "core",
            r#"
[pull_requests]
allow_squash_merge = true
allow_rebase_merge = false
"#,
        );
        let infra = set_with_repo_settings(
            "infra",
            r#"
[pull_requests]
allow_squash_merge = false
allow_rebase_merge = true
"#,
        );

        let reason = merge_sets_for_repo(&[core, infra]).unwrap_err().to_string();
        let error = crate::error::Error::MergeConflict {
            repo: "infra".to_string(),
            reason,
        };

        assert_eq!(
            error.to_string(),
            "Repo 'infra' has conflicting config:\n  repo settings (pull requests):\n    conflicting \
             sets: set 'core' and set 'infra'\n    differing values:\n      - allow_squash_merge: \
             true vs false\n      - allow_rebase_merge: false vs true"
        );
    }

    #[test]
    fn unions_required_status_checks_from_multiple_sets() {
        let title_check = set_with_repo_settings(
            "title-check",
            r#"
[[branch_protection.rules]]
pattern = "main"

[branch_protection.rules.required_status_checks]
contexts = ["check-title / check-title"]

[[branch_protection.rules.required_status_checks.checks]]
context = "check-title / check-title"
app_id = 15368
"#,
        );
        let gate = set_with_repo_settings(
            "gate",
            r#"
[[branch_protection.rules]]
pattern = "main"

[branch_protection.rules.required_status_checks]
contexts = ["gate"]

[[branch_protection.rules.required_status_checks.checks]]
context = "gate"
app_id = 15368
"#,
        );

        let merged = merge_sets_for_repo(&[title_check, gate]).unwrap();
        let status_checks = merged
            .repo_settings
            .unwrap()
            .branch_protection
            .unwrap()
            .rules
            .remove(0)
            .required_status_checks
            .unwrap();

        assert_eq!(
            status_checks.contexts.unwrap(),
            vec!["check-title / check-title", "gate"]
        );
        assert_eq!(
            status_checks
                .checks
                .unwrap()
                .into_iter()
                .map(|check| (check.context, check.app_id))
                .collect::<Vec<_>>(),
            vec![
                ("check-title / check-title".to_string(), Some(15368)),
                ("gate".to_string(), Some(15368)),
            ]
        );
    }

    #[test]
    fn additive_list_merge_preserves_order_and_removes_duplicates() {
        assert_eq!(
            merge_option_vec(
                Some(vec!["check-title", "gate", "check-title"]),
                Some(vec!["gate", "security"]),
            ),
            Some(vec!["check-title", "gate", "security"])
        );
    }

    #[test]
    fn still_rejects_conflicting_scalar_branch_protection_settings() {
        let strict = set_with_repo_settings(
            "strict",
            r#"
[[branch_protection.rules]]
pattern = "main"

[branch_protection.rules.required_status_checks]
strict = true
"#,
        );
        let not_strict = set_with_repo_settings(
            "not-strict",
            r#"
[[branch_protection.rules]]
pattern = "main"

[branch_protection.rules.required_status_checks]
strict = false
"#,
        );

        let error = merge_sets_for_repo(&[strict, not_strict]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "repo settings (branch protection):\n    conflicting sets: set 'strict' and set \
             'not-strict'\n    rule: 'main'\n    conflict: required_status_checks: strict is true in \
             the previously merged configuration vs false in the incoming set"
        );
    }
}
