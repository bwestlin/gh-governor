use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::util::{SUPPORTED_EXTS, parse_by_extension};

#[derive(Debug, Deserialize, Clone)]
pub struct RepoConfig {
    pub name: String,
    #[serde(default)]
    pub sets: Vec<String>,
}

/// Root configuration read from `gh-governor-conf.{toml,yml,yaml,json}`.
#[derive(Debug, Deserialize, Clone)]
pub struct RootConfig {
    /// GitHub organization to operate on.
    pub org: String,
    /// Sets applied to every repository unless overridden.
    #[serde(default)]
    pub default_sets: Vec<String>,
    /// Repositories and their per-repo set ordering.
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    /// Optional directory for configuration sets (relative to base); defaults to `config-sets/`.
    #[serde(default)]
    pub config_sets_dir: Option<String>,
}

const MAIN_CONFIG_BASENAME: &str = "gh-governor-conf";

pub fn load_root_config(base: &Path) -> Result<(RootConfig, PathBuf)> {
    let path = find_main_config(base)?;
    let contents = fs::read_to_string(&path).map_err(|e| Error::io_with_path(e, path.clone()))?;
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
    Err(Error::MissingConfig {
        base: base.to_path_buf(),
    })
}
