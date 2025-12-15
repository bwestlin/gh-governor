use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error{path}: {source}")]
    Io {
        #[source]
        source: std::io::Error,
        path: String,
    },
    #[error("failed to parse toml{path}: {source}")]
    Toml {
        #[source]
        source: toml::de::Error,
        path: String,
    },
    #[error("failed to parse yaml{path}: {source}")]
    Yaml {
        #[source]
        source: serde_yaml::Error,
        path: String,
    },
    #[error("failed to parse json{path}: {source}")]
    Json {
        #[source]
        source: serde_json::Error,
        path: String,
    },
    #[error("unsupported config extension '{ext}' in {path}")]
    UnsupportedExtension { ext: String, path: PathBuf },
    #[error(
        "no main config file found at {base} (looked for gh-governor-conf.{{toml,yml,yaml,json}})"
    )]
    MissingConfig { base: PathBuf },
    #[error("glob pattern error: {0}")]
    GlobPattern(#[from] glob::PatternError),
    #[error("glob error reading paths: {0}")]
    GlobGlob(#[from] glob::GlobError),
    #[error("github api error: {0}")]
    Octo(#[from] octocrab::Error),
    #[error("repository '{org}/{repo}' not found")]
    RepoNotFound { org: String, repo: String },
    #[error("repo '{repo}' has conflicting config: {reason}")]
    MergeConflict { repo: String, reason: String },
}

impl Error {
    pub fn io_with_path(source: std::io::Error, path: PathBuf) -> Self {
        Error::Io {
            source,
            path: format!(" at {}", path.display()),
        }
    }

    pub fn toml_with_path(source: toml::de::Error, path: PathBuf) -> Self {
        Error::Toml {
            source,
            path: format!(" in {}", path.display()),
        }
    }

    pub fn yaml_with_path(source: serde_yaml::Error, path: PathBuf) -> Self {
        Error::Yaml {
            source,
            path: format!(" in {}", path.display()),
        }
    }

    pub fn json_with_path(source: serde_json::Error, path: PathBuf) -> Self {
        Error::Json {
            source,
            path: format!(" in {}", path.display()),
        }
    }
}
