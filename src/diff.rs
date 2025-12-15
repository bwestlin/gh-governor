use octocrab::models::Label;

use crate::sets::LabelSpec;
use crate::settings::RepoSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelDiff {
    pub to_add: Vec<LabelSpec>,
    pub to_update: Vec<LabelSpec>,
    pub to_remove: Vec<LabelSpec>,
}

pub fn diff_labels(desired: &[LabelSpec], current: &[Label]) -> LabelDiff {
    let mut to_add = Vec::new();
    let mut to_update = Vec::new();
    let mut to_remove = Vec::new();

    for want in desired {
        match current.iter().find(|c| c.name == want.name) {
            None => to_add.push(want.clone()),
            Some(existing) => {
                let same_color =
                    normalize_color(&Some(existing.color.clone())) == normalize_color(&want.color);
                let same_desc = existing.description.as_deref() == want.description.as_deref();
                if !same_color || !same_desc {
                    to_update.push(want.clone());
                }
            }
        }
    }

    for existing in current {
        if !desired.iter().any(|d| d.name == existing.name) {
            to_remove.push(LabelSpec {
                name: existing.name.clone(),
                color: normalize_color(&Some(existing.color.clone())),
                description: existing.description.clone(),
            });
        }
    }

    to_add.sort_by(|a, b| a.name.cmp(&b.name));
    to_update.sort_by(|a, b| a.name.cmp(&b.name));
    to_remove.sort_by(|a, b| a.name.cmp(&b.name));

    LabelDiff {
        to_add,
        to_update,
        to_remove,
    }
}

fn normalize_color(color: &Option<String>) -> Option<String> {
    color
        .as_ref()
        .map(|c| c.trim_start_matches('#').to_lowercase())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSettingsDiff {
    pub changes: Vec<SettingChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    pub field: &'static str,
    pub current: Option<bool>,
    pub desired: bool,
}

pub fn diff_repo_settings(desired: &RepoSettings, current: &RepoSettings) -> RepoSettingsDiff {
    let mut changes = Vec::new();

    let desired_pr = match &desired.pull_requests {
        Some(pr) => pr,
        None => {
            return RepoSettingsDiff { changes };
        }
    };
    let current_pr = current.pull_requests.as_ref();

    let mut check = |field: &'static str, want: Option<bool>, have: Option<bool>| {
        if let Some(target) = want {
            if have != Some(target) {
                changes.push(SettingChange {
                    field,
                    current: have,
                    desired: target,
                });
            }
        }
    };

    check(
        "allow_merge_commit",
        desired_pr.allow_merge_commit,
        current_pr.and_then(|p| p.allow_merge_commit),
    );
    check(
        "allow_squash_merge",
        desired_pr.allow_squash_merge,
        current_pr.and_then(|p| p.allow_squash_merge),
    );
    check(
        "allow_rebase_merge",
        desired_pr.allow_rebase_merge,
        current_pr.and_then(|p| p.allow_rebase_merge),
    );
    check(
        "allow_auto_merge",
        desired_pr.allow_auto_merge,
        current_pr.and_then(|p| p.allow_auto_merge),
    );
    check(
        "delete_branch_on_merge",
        desired_pr.delete_branch_on_merge,
        current_pr.and_then(|p| p.delete_branch_on_merge),
    );

    RepoSettingsDiff { changes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbl(name: &str, color: Option<&str>, desc: Option<&str>) -> LabelSpec {
        LabelSpec {
            name: name.to_string(),
            color: color.map(|s| s.to_string()),
            description: desc.map(|s| s.to_string()),
        }
    }

    #[test]
    fn computes_add_and_update() {
        let desired = vec![
            lbl("bug", Some("ff0000"), Some("bug")),
            lbl("new", None, None),
        ];
        let current: Vec<Label> = serde_json::from_value(serde_json::json!([
            {
                "id": 1,
                "node_id": "abc",
                "url": "https://example.com",
                "name": "bug",
                "color": "00ff00",
                "default": false,
                "description": "bug"
            },
            {
                "id": 2,
                "node_id": "def",
                "url": "https://example.com",
                "name": "old",
                "color": "aaaaaa",
                "default": false,
                "description": "old"
            }
        ]))
        .unwrap();

        let diff = diff_labels(&desired, &current);
        assert_eq!(diff.to_add.len(), 1);
        assert_eq!(diff.to_update.len(), 1);
        assert_eq!(diff.to_add[0].name, "new");
        assert_eq!(diff.to_update[0].name, "bug");
        assert_eq!(diff.to_remove.len(), 1);
        assert_eq!(diff.to_remove[0].name, "old");
    }

    #[test]
    fn computes_repo_settings_diff() {
        let desired = RepoSettings {
            pull_requests: Some(crate::settings::PullRequestSettings {
                allow_merge_commit: Some(false),
                allow_squash_merge: Some(true),
                allow_rebase_merge: None,
                allow_auto_merge: Some(true),
                delete_branch_on_merge: Some(true),
            }),
        };
        let current = RepoSettings {
            pull_requests: Some(crate::settings::PullRequestSettings {
                allow_merge_commit: Some(true),
                allow_squash_merge: Some(true),
                allow_rebase_merge: Some(true),
                allow_auto_merge: Some(false),
                delete_branch_on_merge: Some(false),
            }),
        };

        let diff = diff_repo_settings(&desired, &current);
        assert_eq!(diff.changes.len(), 3);
        assert!(diff.changes.iter().any(|c| c.field == "allow_merge_commit"
            && c.current == Some(true)
            && c.desired == false));
        assert!(diff.changes.iter().any(|c| c.field == "allow_auto_merge"
            && c.current == Some(false)
            && c.desired == true));
        assert!(
            diff.changes
                .iter()
                .any(|c| c.field == "delete_branch_on_merge"
                    && c.current == Some(false)
                    && c.desired == true)
        );
        // unchanged or unspecified fields should not show up
        assert!(!diff.changes.iter().any(|c| c.field == "allow_squash_merge"));
        assert!(!diff.changes.iter().any(|c| c.field == "allow_rebase_merge"));
    }
}
