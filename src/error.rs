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
    #[error("Failed to parse toml{path}: {source}")]
    Toml {
        #[source]
        source: toml::de::Error,
        path: String,
    },
    #[error("Failed to write toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("Failed to parse yaml{path}: {source}")]
    Yaml {
        #[source]
        source: serde_yaml::Error,
        path: String,
    },
    #[error("Failed to parse json{path}: {source}")]
    Json {
        #[source]
        source: serde_json::Error,
        path: String,
    },
    #[error("Unsupported config extension '{ext}' in {path}")]
    UnsupportedExtension { ext: String, path: PathBuf },
    #[error(
        "No main config file found at {base} (looked for gh-governor-conf.{{toml,yml,yaml,json}})"
    )]
    MissingConfig { base: PathBuf },
    #[error("Failed to serialize yaml: {0}")]
    YamlSer(#[from] serde_yaml::Error),
    #[error("Failed to serialize json: {0}")]
    JsonSer(#[from] serde_json::Error),
    #[error("Glob pattern error: {0}")]
    GlobPattern(#[from] glob::PatternError),
    #[error("Glob error reading paths: {0}")]
    GlobGlob(#[from] glob::GlobError),
    #[error("GitHub API error: {0}")]
    Octo(Box<octocrab::Error>),
    #[error(
        "Repository '{org}/{repo}' was not found or the token cannot access it; verify the repository name, token repository access/scopes, and organization SSO authorization"
    )]
    RepoNotFound { org: String, repo: String },
    #[error(
        "GitHub authentication failed: the token is invalid, expired, or revoked; update --token or GITHUB_TOKEN"
    )]
    AuthenticationFailed,
    #[error("Repo '{repo}' has conflicting config:\n  {reason}")]
    MergeConflict { repo: String, reason: String },
    #[error("I/O error: {0}")]
    IoSimple(#[from] std::io::Error),
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
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

impl From<octocrab::Error> for Error {
    fn from(err: octocrab::Error) -> Self {
        if let octocrab::Error::GitHub { source, .. } = &err
            && source.status_code == http::StatusCode::UNAUTHORIZED
        {
            return Error::AuthenticationFailed;
        }
        Error::Octo(Box::new(err))
    }
}
