use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::sets::LabelSpec;

#[derive(Debug, Deserialize, Clone)]
pub struct OrgDefaults {
    #[serde(default)]
    pub labels: Vec<LabelSpec>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RepoConfig {
    pub name: String,
    #[serde(default)]
    pub sets: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RootConfig {
    pub org: String,
    #[serde(default)]
    pub default_sets: Vec<String>,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub config_sets_dir: Option<String>,
    #[serde(default)]
    pub token_env_var: Option<String>,
    #[serde(default)]
    pub org_defaults: Option<OrgDefaults>,
}

const MAIN_CONFIG_BASENAME: &str = "gh-governor-conf";
const SUPPORTED_EXTS: &[&str] = &["toml", "yml", "yaml", "json"];

pub fn load_root_config(base: &Path) -> Result<(RootConfig, PathBuf)> {
    let path = find_main_config(base)?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    let cfg = parse_by_extension(&path, &contents)?;
    Ok((cfg, path))
}

pub fn resolve_sets_dir(base: &Path, root: &RootConfig) -> PathBuf {
    match &root.config_sets_dir {
        Some(dir) => base.join(dir),
        None => base.join("config-sets"),
    }
}

fn find_main_config(base: &Path) -> Result<PathBuf> {
    for ext in SUPPORTED_EXTS {
        let candidate = base.join(format!("{MAIN_CONFIG_BASENAME}.{ext}"));
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "no main config file found at {} (looked for {}.{{toml,yml,yaml,json}})",
        base.display(),
        MAIN_CONFIG_BASENAME
    ))
}

fn parse_by_extension<T: for<'de> Deserialize<'de>>(path: &Path, contents: &str) -> Result<T> {
    match path
        .extension()
        .and_then(|os| os.to_str())
        .unwrap_or_default()
    {
        "toml" => toml::from_str(contents).context("parsing toml config"),
        "yml" | "yaml" => serde_yaml::from_str(contents).context("parsing yaml config"),
        "json" => serde_json::from_str(contents).context("parsing json config"),
        other => Err(anyhow!("unsupported config extension: {other}")),
    }
}
