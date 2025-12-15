use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use glob::glob;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::settings::{BranchProtectionConfig, RepoSettings};
use crate::util::{SUPPORTED_EXTS, parse_by_extension};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelSpec {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTemplateFile {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ChecksConfig {
    #[serde(default = "ChecksConfig::default_require_codeowners")]
    pub require_codeowners: bool,
    #[serde(default = "ChecksConfig::default_warn_on_inactive")]
    pub warn_on_inactive_owners: bool,
}

impl ChecksConfig {
    fn default_require_codeowners() -> bool {
        true
    }

    fn default_warn_on_inactive() -> bool {
        true
    }
}

impl Default for ChecksConfig {
    fn default() -> Self {
        Self {
            require_codeowners: true,
            warn_on_inactive_owners: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetDefinition {
    pub name: String,
    pub path: PathBuf,
    pub labels: Vec<LabelSpec>,
    pub issue_templates: Vec<IssueTemplateFile>,
    pub repo_settings: Option<RepoSettings>,
    pub branch_protection: Option<BranchProtectionConfig>,
    pub checks: Option<ChecksConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct LabelFields {
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub fn deserialize_label_map<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<LabelSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let map: HashMap<String, LabelFields> = HashMap::deserialize(deserializer)?;
    Ok(labels_from_map(map))
}

fn labels_from_map(map: HashMap<String, LabelFields>) -> Vec<LabelSpec> {
    let mut labels: Vec<_> = map
        .into_iter()
        .map(|(name, fields)| LabelSpec {
            name,
            color: fields.color,
            description: fields.description,
        })
        .collect();
    labels.sort_by(|a, b| a.name.cmp(&b.name));
    labels
}

pub fn load_set(base_dir: &Path, name: &str) -> Result<SetDefinition> {
    let path = base_dir.join(name);
    if !path.is_dir() {
        return Err(Error::MissingConfig { base: path });
    }

    let labels = load_labels_file(&path)?.unwrap_or_default();
    let repo_settings = load_named_file::<RepoSettings>(&path, "repo-settings")?;
    let branch_protection = load_named_file::<BranchProtectionConfig>(&path, "branch-protection")?;
    let checks = load_named_file::<ChecksConfig>(&path, "checks")?;
    let issue_templates = load_issue_templates(&path)?;

    Ok(SetDefinition {
        name: name.to_string(),
        path,
        labels,
        issue_templates,
        repo_settings,
        branch_protection,
        checks,
    })
}

fn load_issue_templates(set_path: &Path) -> Result<Vec<IssueTemplateFile>> {
    let mut templates = Vec::new();
    let template_dir = set_path.join(".github").join("ISSUE_TEMPLATE");
    for ext in ["yml", "yaml"] {
        let pattern = template_dir.join(format!("*.{ext}"));
        for entry in glob(pattern.to_str().unwrap_or_default())? {
            let path = entry.map_err(Error::GlobGlob)?;
            let contents =
                fs::read_to_string(&path).map_err(|e| Error::io_with_path(e, path.clone()))?;
            let mut rel = path.to_string_lossy().to_string();
            if let Some(idx) = rel.find(".github/") {
                rel = rel[idx..].to_string();
            } else if let Ok(stripped) = path.strip_prefix(set_path) {
                rel = stripped.to_string_lossy().to_string();
            }
            templates.push(IssueTemplateFile {
                path: rel,
                contents,
            });
        }
    }
    Ok(templates)
}

fn load_labels_file(dir: &Path) -> Result<Option<Vec<LabelSpec>>> {
    for ext in SUPPORTED_EXTS {
        let candidate = dir.join(format!("labels.{ext}"));
        if candidate.exists() {
            let contents = fs::read_to_string(&candidate)
                .map_err(|e| Error::io_with_path(e, candidate.clone()))?;
            let map: HashMap<String, LabelFields> = parse_by_extension(&candidate, &contents)?;
            return Ok(Some(labels_from_map(map)));
        }
    }
    Ok(None)
}

fn load_named_file<T: for<'de> Deserialize<'de>>(dir: &Path, stem: &str) -> Result<Option<T>> {
    for ext in SUPPORTED_EXTS {
        let candidate = dir.join(format!("{stem}.{ext}"));
        if candidate.exists() {
            let contents = fs::read_to_string(&candidate)
                .map_err(|e| Error::io_with_path(e, candidate.clone()))?;
            let parsed = parse_by_extension(&candidate, &contents)?;
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}
