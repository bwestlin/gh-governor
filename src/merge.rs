use std::collections::HashMap;

use thiserror::Error;

use crate::sets::{ChecksConfig, GithubFile, LabelSpec, SetDefinition};
use crate::settings::{
    BranchProtectionRule, BranchRestrictions, RepoSettings, RequiredPullRequestReviews,
    RequiredStatusChecks, ReviewDismissalRestrictions,
};

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("label conflict for '{0}' between sets; definitions differ")]
    LabelConflict(String),
    #[error(".github file conflict for '{0}' between sets")]
    TemplateConflict(String),
    #[error("{0} conflict between sets")]
    GenericConflict(String),
}

pub type MergeResult<T> = Result<T, MergeError>;

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
    let mut checks: Option<ChecksConfig> = None;

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
            repo_settings = merge_repo_settings(repo_settings, settings.clone())?;
        }

        if let Some(chk) = &set.checks {
            checks = merge_or_conflict(checks, chk.clone(), "checks")?;
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
        checks,
    })
}

fn merge_repo_settings(
    existing: Option<RepoSettings>,
    incoming: RepoSettings,
) -> MergeResult<Option<RepoSettings>> {
    match existing {
        None => Ok(Some(incoming)),
        Some(mut current) => {
            // Merge pull request settings only if they don't conflict.
            match (&current.pull_requests, &incoming.pull_requests) {
                (None, Some(pr)) => current.pull_requests = Some(pr.clone()),
                (Some(_), None) => {}
                (Some(a), Some(b)) if a != b => {
                    return Err(MergeError::GenericConflict(
                        "repo settings (pull requests)".to_string(),
                    ));
                }
                _ => {}
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
                                            "repo settings (branch protection): {}",
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
        contexts: merge_option_vec(current.contexts, incoming.contexts, "contexts")?,
        checks: merge_option_vec(current.checks, incoming.checks, "checks")?,
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
        users: merge_option_vec(current.users, incoming.users, "users")?,
        teams: merge_option_vec(current.teams, incoming.teams, "teams")?,
    })
}

fn merge_branch_restrictions(
    current: BranchRestrictions,
    incoming: BranchRestrictions,
) -> Result<BranchRestrictions, String> {
    Ok(BranchRestrictions {
        users: merge_option_vec(current.users, incoming.users, "users")?,
        teams: merge_option_vec(current.teams, incoming.teams, "teams")?,
        apps: merge_option_vec(current.apps, incoming.apps, "apps")?,
    })
}

fn merge_option_field<T: PartialEq + Copy>(
    current: Option<T>,
    incoming: Option<T>,
    field: &str,
) -> Result<Option<T>, String> {
    match (current, incoming) {
        (None, v) => Ok(v),
        (v, None) => Ok(v),
        (Some(a), Some(b)) if a == b => Ok(Some(a)),
        (Some(_), Some(_)) => Err(format!("conflict on {}", field)),
    }
}

fn merge_option_vec<T: PartialEq>(
    current: Option<Vec<T>>,
    incoming: Option<Vec<T>>,
    field: &str,
) -> Result<Option<Vec<T>>, String> {
    match (current, incoming) {
        (None, v) => Ok(v),
        (v, None) => Ok(v),
        (Some(a), Some(b)) if a == b => Ok(Some(a)),
        (Some(_), Some(_)) => Err(format!("conflict on {}", field)),
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
fn merge_or_conflict<T: PartialEq>(
    existing: Option<T>,
    incoming: T,
    what: &str,
) -> MergeResult<Option<T>> {
    match existing {
        Some(current) if current != incoming => Err(MergeError::GenericConflict(what.to_string())),
        Some(current) => Ok(Some(current)),
        None => Ok(Some(incoming)),
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
}
