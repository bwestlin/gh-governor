use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use crate::config::{RepoConfig, RootConfig};
use crate::error::Result;
use crate::github::GithubClient;
use crate::sets::{IssueTemplateFile, LabelSpec};
use crate::settings::{BranchProtectionRule, RepoSettings};

#[derive(Clone)]
struct RepoSnapshot {
    name: String,
    labels: Vec<LabelSpec>,
    settings: Option<RepoSettings>,
    branch_protection: Option<BranchProtectionRule>,
    templates: Vec<IssueTemplateFile>,
}

pub async fn generate_configs(
    gh: &GithubClient,
    repos: &[String],
    output_base: &Path,
    org: &str,
    verbose: bool,
) -> Result<()> {
    println!(
        "Generating configs for org '{}' into {}",
        org,
        output_base.display()
    );

    let mut snapshots = Vec::new();
    for repo in repos {
        let snap = fetch_repo(gh, repo).await?;
        if verbose {
            println!(
                "  fetched {}: labels {}, templates {}, settings {}, branch protection {}",
                repo,
                snap.labels.len(),
                snap.templates.len(),
                snap.settings.as_ref().map(|_| "yes").unwrap_or("no"),
                snap.branch_protection
                    .as_ref()
                    .map(|_| "yes")
                    .unwrap_or("no")
            );
        }
        snapshots.push(snap);
    }

    if snapshots.is_empty() {
        return Ok(());
    }

    let common_labels = compute_common_labels(&snapshots);
    let common_settings = compute_common_settings(&snapshots);
    let common_bp = compute_common_branch_protection(&snapshots);
    let mut common_templates = compute_common_templates(&snapshots);
    let base_config = snapshots.iter().find_map(|s| {
        s.templates
            .iter()
            .find(|t| t.path.ends_with("config.yml"))
            .cloned()
    });
    ensure_config_for_templates(&mut common_templates, base_config.as_ref());

    let mut root = RootConfig {
        org: org.to_string(),
        default_sets: Vec::new(),
        repos: Vec::new(),
        config_sets_dir: None,
    };

    let sets_root = output_base.join("config-sets");
    if !common_labels.is_empty()
        || common_settings.is_some()
        || common_bp.is_some()
        || !common_templates.is_empty()
    {
        let core_dir = sets_root.join("core");
        write_set(
            &core_dir,
            &common_labels,
            common_settings.as_ref(),
            common_bp.as_ref(),
            &common_templates,
        )?;
        root.default_sets.push("core".to_string());
        if verbose {
            println!(
                "  core set: labels {}, templates {}, settings {}, branch protection {}",
                common_labels.len(),
                common_templates.len(),
                common_settings.as_ref().map(|_| "yes").unwrap_or("no"),
                common_bp.as_ref().map(|_| "yes").unwrap_or("no")
            );
        }
    }

    for snap in snapshots {
        let mut labels = snap.labels.clone();
        labels.retain(|l| !common_labels.iter().any(|c| c.name == l.name));

        let settings = match (&snap.settings, &common_settings) {
            (Some(s), Some(common)) if s != common => Some(s.clone()),
            (Some(s), None) => Some(s.clone()),
            _ => None,
        };

        let bp = match (&snap.branch_protection, &common_bp) {
            (Some(b), Some(common)) if b != common => Some(b.clone()),
            (Some(b), None) => Some(b.clone()),
            _ => None,
        };

        let mut templates = snap.templates.clone();
        templates.retain(|t| {
            !common_templates
                .iter()
                .any(|c| c.path == t.path && c.contents == t.contents)
        });
        templates.retain(|t| !t.path.ends_with("config.yml"));

        let mut sets = Vec::new();
        if !labels.is_empty() || settings.is_some() || bp.is_some() || !templates.is_empty() {
            let set_dir = sets_root.join(&snap.name);
            write_set(
                &set_dir,
                &labels,
                settings.as_ref(),
                bp.as_ref(),
                &templates,
            )?;
            sets.push(snap.name.clone());
            if verbose {
                println!(
                    "  set {}: labels {}, templates {}, settings {}, branch protection {}",
                    snap.name,
                    labels.len(),
                    templates.len(),
                    settings.as_ref().map(|_| "yes").unwrap_or("no"),
                    bp.as_ref().map(|_| "yes").unwrap_or("no")
                );
            }
        }

        if !root.default_sets.is_empty() {
            sets.insert(0, "core".to_string());
        }

        root.repos.push(RepoConfig {
            name: snap.name.clone(),
            sets,
        });
    }

    fs::create_dir_all(output_base)?;
    let root_path = output_base.join("gh-governor-conf.toml");
    let root_contents = toml::to_string_pretty(&root)?;
    fs::write(&root_path, root_contents)?;
    println!("Done. Root config written to {}", root_path.display());

    Ok(())
}

async fn fetch_repo(gh: &GithubClient, repo: &str) -> Result<RepoSnapshot> {
    let info = gh.get_repo(repo).await?;
    let default_branch = info
        .default_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());

    let labels = gh.list_repo_labels(repo).await?;
    let settings = gh.get_repo_settings(repo).await?;
    let bp = gh.get_branch_protection(repo, &default_branch).await?;

    let mut templates = Vec::new();
    let paths = gh
        .list_github_files(repo, &default_branch, ".github/ISSUE_TEMPLATE/")
        .await
        .unwrap_or_default();
    for path in paths {
        if let Some(file) = gh.get_file(repo, &path, Some(&default_branch)).await? {
            templates.push(IssueTemplateFile {
                path,
                contents: file.content,
            });
        }
    }

    Ok(RepoSnapshot {
        name: repo.to_string(),
        labels: labels
            .into_iter()
            .map(|l| LabelSpec {
                name: l.name,
                color: Some(l.color),
                description: l.description,
            })
            .collect(),
        settings: Some(settings),
        branch_protection: bp,
        templates,
    })
}

fn compute_common_labels(snapshots: &[RepoSnapshot]) -> Vec<LabelSpec> {
    if snapshots.is_empty() {
        return Vec::new();
    }
    let mut common = snapshots[0].labels.clone();
    common.retain(|lbl| {
        snapshots.iter().all(|s| {
            s.labels.iter().any(|l| {
                l.name == lbl.name && l.color == lbl.color && l.description == lbl.description
            })
        })
    });
    common.sort_by(|a, b| a.name.cmp(&b.name));
    common
}

fn compute_common_settings(snapshots: &[RepoSnapshot]) -> Option<RepoSettings> {
    if snapshots.is_empty() {
        return None;
    }
    let first = snapshots[0].settings.clone()?;
    if snapshots
        .iter()
        .all(|s| s.settings.as_ref() == Some(&first))
    {
        Some(first)
    } else {
        None
    }
}

fn compute_common_branch_protection(snapshots: &[RepoSnapshot]) -> Option<BranchProtectionRule> {
    if snapshots.is_empty() {
        return None;
    }
    let first = snapshots[0].branch_protection.clone()?;
    if snapshots
        .iter()
        .all(|s| s.branch_protection.as_ref() == Some(&first))
    {
        Some(first)
    } else {
        None
    }
}

fn compute_common_templates(snapshots: &[RepoSnapshot]) -> Vec<IssueTemplateFile> {
    if snapshots.is_empty() {
        return Vec::new();
    }
    let mut common_map: HashMap<String, String> = HashMap::new();
    for tpl in &snapshots[0].templates {
        if snapshots.iter().all(|s| {
            s.templates
                .iter()
                .any(|t| t.path == tpl.path && t.contents == tpl.contents)
        }) {
            common_map.insert(tpl.path.clone(), tpl.contents.clone());
        }
    }
    let mut common: Vec<IssueTemplateFile> = common_map
        .into_iter()
        .map(|(path, contents)| IssueTemplateFile { path, contents })
        .collect();
    common.sort_by(|a, b| a.path.cmp(&b.path));
    common
}

fn ensure_config_for_templates(
    templates: &mut Vec<IssueTemplateFile>,
    base_config: Option<&IssueTemplateFile>,
) {
    let has_config = templates
        .iter()
        .any(|t| t.path.ends_with(".github/ISSUE_TEMPLATE/config.yml"));
    if has_config {
        return;
    }
    let entries: Vec<TemplateConfigEntry> = templates
        .iter()
        .filter(|t| !t.path.ends_with("config.yml"))
        .map(|t| {
            let file = file_name(&t.path).unwrap_or_else(|| t.path.clone());
            TemplateConfigEntry {
                file,
                name: None,
                description: None,
            }
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    let mut cfg = base_config
        .and_then(|c| serde_yaml::from_str::<TemplateConfig>(c.contents.as_str()).ok())
        .unwrap_or_else(|| TemplateConfig {
            blank_issues_enabled: None,
            contact_links: None,
            issue_templates: None,
        });
    // Preserve names/descriptions where file matches; otherwise leave None.
    let mut merged_entries = Vec::new();
    for entry in entries {
        if let Some(existing) = cfg
            .issue_templates
            .as_ref()
            .and_then(|v| v.iter().find(|e| e.file == entry.file))
        {
            merged_entries.push(TemplateConfigEntry {
                file: entry.file,
                name: existing.name.clone(),
                description: existing.description.clone(),
            });
        } else {
            merged_entries.push(entry);
        }
    }
    cfg.issue_templates = None;
    if let Ok(contents) = serde_yaml::to_string(&cfg) {
        templates.push(IssueTemplateFile {
            path: ".github/ISSUE_TEMPLATE/config.yml".to_string(),
            contents,
        });
    }
}

fn file_name(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[derive(Serialize, Deserialize)]
struct TemplateConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    blank_issues_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contact_links: Option<Vec<ContactLink>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_templates: Option<Vec<TemplateConfigEntry>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct TemplateConfigEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    file: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ContactLink {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    about: Option<String>,
}

fn write_set(
    dir: &Path,
    labels: &[LabelSpec],
    settings: Option<&RepoSettings>,
    bp: Option<&BranchProtectionRule>,
    templates: &[IssueTemplateFile],
) -> Result<()> {
    fs::create_dir_all(dir)?;

    if !labels.is_empty() {
        let mut map: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for lbl in labels {
            let mut fields = BTreeMap::new();
            if let Some(color) = &lbl.color {
                fields.insert("color".to_string(), color.clone());
            }
            if let Some(desc) = &lbl.description {
                fields.insert("description".to_string(), desc.clone());
            }
            map.insert(lbl.name.clone(), fields);
        }
        let contents = toml::to_string_pretty(&map)?;
        fs::write(dir.join("labels.toml"), contents)?;
    }

    if let Some(settings) = settings {
        let contents = toml::to_string_pretty(settings)?;
        fs::write(dir.join("repo-settings.toml"), contents)?;
    }

    if let Some(bp) = bp {
        let contents = toml::to_string_pretty(bp)?;
        fs::write(dir.join("branch-protection.toml"), contents)?;
    }

    for tpl in templates {
        let path = dir.join(&tpl.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, tpl.contents.as_bytes())?;
    }

    Ok(())
}
