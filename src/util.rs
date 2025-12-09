use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

pub const SUPPORTED_EXTS: &[&str] = &["toml", "yml", "yaml", "json"];

pub fn parse_by_extension<T: for<'de> Deserialize<'de>>(path: &Path, contents: &str) -> Result<T> {
    match path
        .extension()
        .and_then(|os| os.to_str())
        .unwrap_or_default()
    {
        "toml" => {
            toml::from_str(contents).map_err(|e| Error::toml_with_path(e, path.to_path_buf()))
        }
        "yml" | "yaml" => {
            serde_yaml::from_str(contents).map_err(|e| Error::yaml_with_path(e, path.to_path_buf()))
        }
        "json" => {
            serde_json::from_str(contents).map_err(|e| Error::json_with_path(e, path.to_path_buf()))
        }
        other => Err(Error::UnsupportedExtension {
            ext: other.to_string(),
            path: path.to_path_buf(),
        }),
    }
}
