use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::error::Error;
use crate::error::Result;
use crate::util::SUPPORTED_EXTS;
use crate::util::parse_by_extension;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RepoConfig {
    pub name: String,
    /// Whether organization-wide default sets are applied before this repository's sets.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub inherit_default_sets: bool,
    #[serde(default)]
    pub sets: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Root configuration read from `gh-governor-conf.{toml,yml,yaml,json}`.
#[derive(Debug, Deserialize, Serialize, Clone)]
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

impl RootConfig {
    pub fn sets_for_repo<'a>(&'a self, repo: &'a RepoConfig) -> impl Iterator<Item = &'a String> {
        self.default_sets
            .iter()
            .filter(move |_| repo.inherit_default_sets)
            .chain(repo.sets.iter())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_inherit_default_sets_by_default() {
        let config: RootConfig = toml::from_str(
            r#"
org = "example"
default_sets = ["core"]

[[repos]]
name = "api"
sets = ["api"]
"#,
        )
        .unwrap();

        assert!(config.repos[0].inherit_default_sets);
        assert_eq!(
            config
                .sets_for_repo(&config.repos[0])
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["core", "api"]
        );
    }

    #[test]
    fn repo_can_disable_default_set_inheritance() {
        let config: RootConfig = toml::from_str(
            r#"
org = "example"
default_sets = ["core"]

[[repos]]
name = "infra"
inherit_default_sets = false
sets = ["infra"]
"#,
        )
        .unwrap();

        assert!(!config.repos[0].inherit_default_sets);
        assert_eq!(
            config
                .sets_for_repo(&config.repos[0])
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["infra"]
        );
    }

    #[test]
    fn serialization_only_includes_disabled_inheritance() {
        let mut repo = RepoConfig {
            name: "infra".to_string(),
            inherit_default_sets: true,
            sets: vec!["infra".to_string()],
        };

        assert!(
            !toml::to_string(&repo)
                .unwrap()
                .contains("inherit_default_sets")
        );

        repo.inherit_default_sets = false;
        assert!(
            toml::to_string(&repo)
                .unwrap()
                .contains("inherit_default_sets = false")
        );
    }
}
