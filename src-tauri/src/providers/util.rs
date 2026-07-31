use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::ProviderError;

/// Read and deserialize a JSON credential/config file.
/// A missing file is `NotConfigured`, a broken one is `Parse`.
pub fn read_json_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, ProviderError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|_| ProviderError::NotConfigured)?;
    serde_json::from_str(&text)
        .map_err(|e| ProviderError::Parse(format!("{}: {e}", path.display())))
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

/// `%USERPROFILE%\<name>`, unless `env_override` names an env var that is set.
pub fn home_subdir(name: &str, env_override: Option<&str>) -> PathBuf {
    if let Some(var) = env_override {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    home_dir().join(name)
}

pub fn local_app_data() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir)
}

/// used / limit as a percentage, clamped to 0..=100. Zero limit reads as 0.
pub fn percent(used: f64, limit: f64) -> f64 {
    if limit <= 0.0 {
        return 0.0;
    }
    (used / limit * 100.0).clamp(0.0, 100.0)
}

pub fn remaining_percent(used_percent: f64) -> f64 {
    (100.0 - used_percent).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_edges() {
        assert_eq!(percent(0.0, 0.0), 0.0);
        assert_eq!(percent(5.0, 10.0), 50.0);
        assert_eq!(percent(20.0, 10.0), 100.0);
        assert_eq!(remaining_percent(150.0), 0.0);
    }
}
