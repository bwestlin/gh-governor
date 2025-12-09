use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use glob::glob;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelSpec {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
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
    pub repo_settings: Option<serde_json::Value>,
    pub branch_protection: Option<serde_json::Value>,
    pub checks: Option<ChecksConfig>,
}

const SUPPORTED_EXTS: &[&str] = &["toml", "yml", "yaml", "json"];

pub fn load_set(base_dir: &Path, name: &str) -> Result<SetDefinition> {
    let path = base_dir.join(name);
    if !path.is_dir() {
        return Err(anyhow!(
            "configuration set '{}' not found at {}",
            name,
            path.display()
        ));
    }

    let labels = load_named_file::<Vec<LabelSpec>>(&path, "labels")?.unwrap_or_default();
    let repo_settings = load_named_file::<serde_json::Value>(&path, "repo-settings")?;
    let branch_protection = load_named_file::<serde_json::Value>(&path, "branch-protection")?;
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
            let path = entry?;
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("reading issue template {}", path.display()))?;
            let rel = path
                .strip_prefix(set_path)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            templates.push(IssueTemplateFile {
                path: rel,
                contents,
            });
        }
    }
    Ok(templates)
}

fn load_named_file<T: for<'de> Deserialize<'de>>(dir: &Path, stem: &str) -> Result<Option<T>> {
    for ext in SUPPORTED_EXTS {
        let candidate = dir.join(format!("{stem}.{ext}"));
        if candidate.exists() {
            let contents = fs::read_to_string(&candidate).with_context(|| {
                format!("failed to read file {} for set", candidate.display())
            })?;
            let parsed = parse_by_extension(&candidate, &contents)?;
            return Ok(Some(parsed));
        }
    }
    Ok(None)
}

fn parse_by_extension<T: for<'de> Deserialize<'de>>(path: &Path, contents: &str) -> Result<T> {
    match path
        .extension()
        .and_then(|os| os.to_str())
        .unwrap_or_default()
    {
        "toml" => toml::from_str(contents).context("parsing toml file"),
        "yml" | "yaml" => serde_yaml::from_str(contents).context("parsing yaml file"),
        "json" => serde_json::from_str(contents).context("parsing json file"),
        other => Err(anyhow!("unsupported extension {other} in {}", path.display())),
    }
}
