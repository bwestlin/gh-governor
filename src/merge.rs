use std::collections::HashMap;

use thiserror::Error;

use crate::sets::{ChecksConfig, IssueTemplateFile, LabelSpec, SetDefinition};
use crate::settings::{BranchProtectionConfig, RepoSettings};

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("label conflict for '{0}' between sets; definitions differ")]
    LabelConflict(String),
    #[error("issue template conflict for '{0}' between sets")]
    TemplateConflict(String),
    #[error("{0} conflict between sets")]
    GenericConflict(String),
}

pub type MergeResult<T> = Result<T, MergeError>;

#[derive(Debug, Clone)]
pub struct MergedRepoConfig {
    pub labels: Vec<LabelSpec>,
    pub issue_templates: Vec<IssueTemplateFile>,
    pub repo_settings: Option<RepoSettings>,
    pub branch_protection: Option<BranchProtectionConfig>,
    pub checks: Option<ChecksConfig>,
}

pub fn merge_sets_for_repo(sets: &[SetDefinition]) -> MergeResult<MergedRepoConfig> {
    let mut labels = HashMap::new();
    let mut templates = HashMap::new();
    let mut repo_settings: Option<RepoSettings> = None;
    let mut branch_protection: Option<BranchProtectionConfig> = None;
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

        for template in &set.issue_templates {
            if templates.contains_key(&template.path) {
                return Err(MergeError::TemplateConflict(template.path.clone()));
            }
            templates.insert(template.path.clone(), template.clone());
        }

        if let Some(settings) = &set.repo_settings {
            repo_settings = merge_or_conflict(repo_settings, settings.clone(), "repo settings")?;
        }

        if let Some(bp) = &set.branch_protection {
            branch_protection =
                merge_or_conflict(branch_protection, bp.clone(), "branch protection")?;
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
        issue_templates: templates.into_values().collect(),
        repo_settings,
        branch_protection,
        checks,
    })
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
    use crate::sets::{IssueTemplateFile, SetDefinition};

    fn base_set(name: &str) -> SetDefinition {
        SetDefinition {
            name: name.to_string(),
            path: "".into(),
            labels: Vec::new(),
            issue_templates: Vec::new(),
            repo_settings: None,
            branch_protection: None,
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
        a.issue_templates.push(IssueTemplateFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: String::new(),
        });
        let mut b = base_set("b");
        b.issue_templates.push(IssueTemplateFile {
            path: ".github/ISSUE_TEMPLATE/bug.yml".to_string(),
            contents: String::new(),
        });
        assert!(matches!(
            merge_sets_for_repo(&[a, b]),
            Err(MergeError::TemplateConflict(_))
        ));
    }
}
