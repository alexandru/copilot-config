//! Configuration file boundaries and the domain error type.
//!
//! Configuration documents use JSON5 syntax, so comments, trailing commas, and
//! unquoted keys are accepted. Every document must have an object at its root.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

/// The preset document, relative to the configuration directory.
pub(crate) const PRESETS_FILE: &str = "config.presets.jsonc";

/// Output files paired with the shared fragment each one is merged into.
pub(crate) const OUTPUT_CONFIGS: [(&str, &str); 2] = [
    ("settings.json", "settings.common.jsonc"),
    ("mcp-config.json", "mcp-config.common.jsonc"),
];

/// A configuration or preset problem reported as one message.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    /// The requested file does not exist.
    #[error("configuration file missing: {path}")]
    MissingConfiguration {
        /// Path that was read.
        path: PathBuf,
    },

    /// The file exists but cannot be read as UTF-8 text.
    #[error("failed to read {path}: {source}")]
    ReadConfiguration {
        /// Path that was read.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// The file is not valid JSON5.
    #[error("failed to parse {path}: {source}")]
    ParseConfiguration {
        /// Path that was parsed.
        path: PathBuf,
        /// Parser diagnostic.
        #[source]
        source: json5::Error,
    },

    /// The document root is not a JSON object.
    #[error("{path} must contain a JSON object")]
    NonObjectConfiguration {
        /// Path whose root was rejected.
        path: PathBuf,
    },

    /// A generated value could not be serialized as JSON.
    #[error("failed to serialize {path}: {source}")]
    SerializeConfiguration {
        /// Output path being serialized.
        path: PathBuf,
        /// Serializer failure.
        #[source]
        source: serde_json::Error,
    },

    /// An output file could not be written.
    #[error("failed to write {path}: {source}")]
    WriteConfiguration {
        /// Output path that was written.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// `extends` is neither absent nor an array of strings.
    #[error("Preset '{name}' has invalid 'extends'; expected an array of preset names")]
    InvalidExtends {
        /// Preset containing the invalid field.
        name: String,
    },

    /// A parent named by `extends` does not exist.
    #[error("Preset '{name}' not found in {}", PRESETS_FILE)]
    MissingPreset {
        /// Missing preset name.
        name: String,
    },

    /// A referenced preset exists but is not an object.
    #[error("Preset '{name}' must be an object")]
    NonObjectPreset {
        /// Rejected preset name.
        name: String,
    },

    /// Inheritance revisits a preset on the current resolution path.
    #[error("Circular preset inheritance detected: {chain}")]
    CircularInheritance {
        /// Resolution path from the first repeated preset back to itself.
        chain: String,
    },

    /// A resolved preset does not define one of the required output objects.
    #[error("Preset '{preset}' must define '{output}' as an object")]
    MissingPresetOutput {
        /// Selected preset name.
        preset: String,
        /// Required output key.
        output: String,
    },

    /// The selected name is not a key in the preset document.
    #[error("Preset '{name}' not found. Available presets:\n{available}")]
    UnknownPreset {
        /// Requested preset name.
        name: String,
        /// Visible preset list shown to the user.
        available: String,
    },

    /// An output disappeared between generation and summary construction.
    #[error("generated output missing: {name}")]
    MissingGeneratedOutput {
        /// Required generated output name.
        name: String,
    },

    /// The configuration directory cannot be derived from the executable path.
    #[error("cannot determine the configuration directory: {reason}")]
    ConfigurationDirectory {
        /// Why resolution failed.
        reason: String,
    },
}

/// Reads the root object of a JSON5 file.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file is missing, cannot be read, is not
/// valid JSON5, or does not contain an object at its root.
pub(crate) fn read_jsonc_object(path: &Path) -> Result<Map<String, Value>, ConfigError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::MissingConfiguration {
                path: path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ConfigError::ReadConfiguration {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let value: Value =
        json5::from_str(&content).map_err(|source| ConfigError::ParseConfiguration {
            path: path.to_path_buf(),
            source,
        })?;

    match value {
        Value::Object(object) => Ok(object),
        _ => Err(ConfigError::NonObjectConfiguration {
            path: path.to_path_buf(),
        }),
    }
}

/// Writes a JSON object with two-space indentation, unescaped Unicode text, and
/// one trailing newline.
///
/// # Errors
///
/// Returns [`ConfigError`] when serializing the value or writing the file fails.
pub(crate) fn write_json_object(
    path: &Path,
    value: &Map<String, Value>,
) -> Result<(), ConfigError> {
    let mut content =
        serde_json::to_vec_pretty(value).map_err(|source| ConfigError::SerializeConfiguration {
            path: path.to_path_buf(),
            source,
        })?;
    content.push(b'\n');

    fs::write(path, content).map_err(|source| ConfigError::WriteConfiguration {
        path: path.to_path_buf(),
        source,
    })
}

/// Resolves the repository root from the canonical executable location.
///
/// The installed binary lives in `<root>/bin`, so the root is two parent
/// directories above the canonical executable. Resolving the executable, not
/// the working directory, keeps symlinked invocations consistent: the symlink
/// target determines which repository configuration is used.
///
/// # Errors
///
/// Returns [`ConfigError`] when the executable path cannot be resolved or does
/// not have two parent directories.
pub(crate) fn config_dir() -> Result<PathBuf, ConfigError> {
    let executable =
        std::env::current_exe().map_err(|error| ConfigError::ConfigurationDirectory {
            reason: format!("cannot resolve the current executable: {error}"),
        })?;
    let executable =
        executable
            .canonicalize()
            .map_err(|error| ConfigError::ConfigurationDirectory {
                reason: format!("cannot canonicalize {}: {error}", executable.display()),
            })?;
    let bin = executable
        .parent()
        .ok_or_else(|| ConfigError::ConfigurationDirectory {
            reason: format!("{} has no parent directory", executable.display()),
        })?;
    bin.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ConfigError::ConfigurationDirectory {
            reason: format!("{} has no repository root", bin.display()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_comments_and_trailing_commas() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("config.jsonc");
        fs::write(&path, "{\n  // fragment\n  \"enabled\": true,\n}").expect("write configuration");

        let document = read_jsonc_object(&path).expect("configuration should parse");

        assert_eq!(Value::Object(document), json!({"enabled": true}));
    }

    #[test]
    fn rejects_non_object_roots() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("config.jsonc");
        fs::write(&path, "[1]").expect("write configuration");

        let error = read_jsonc_object(&path).expect_err("array root must be rejected");

        assert!(matches!(error, ConfigError::NonObjectConfiguration { .. }));
    }

    #[test]
    fn missing_files_name_the_requested_path() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("missing.jsonc");

        let error = read_jsonc_object(&path).expect_err("missing file must be rejected");

        assert_eq!(
            error.to_string(),
            format!("configuration file missing: {}", path.display())
        );
    }

    #[test]
    fn writes_two_space_unicode_json_with_a_trailing_newline() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        let path = directory.path().join("output.json");
        let mut document = Map::new();
        document.insert("message".to_owned(), Value::String("héllo ✓".to_owned()));
        document.insert("items".to_owned(), json!([1, true]));
        document.insert("empty".to_owned(), json!({}));

        write_json_object(&path, &document).expect("output should be written");

        assert_eq!(
            fs::read_to_string(&path).expect("read output"),
            "{\n  \"message\": \"héllo ✓\",\n  \"items\": [\n    1,\n    true\n  ],\n  \"empty\": {}\n}\n"
        );
    }
}
