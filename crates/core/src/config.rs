use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const WINDOWS_CONFIG_DIR: &str = "VoxGolem";
const WINDOWS_CONFIG_FILE: &str = "config.toml";
const DEFAULT_SILERO_VAD_MODEL: &str = "models/silero-vad.onnx";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    wake_word_dir: PathBuf,
    parakeet_model_dir: PathBuf,
    silero_vad_model: Option<PathBuf>,
    opencode_path: PathBuf,
    #[serde(default, rename = "start_listening_cue")]
    _start_listening_cue: Option<PathBuf>,
    #[serde(default, rename = "stop_listening_cue")]
    _stop_listening_cue: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub wake_word_dir: PathBuf,
    pub parakeet_model_dir: PathBuf,
    pub silero_vad_model: PathBuf,
    pub opencode_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingAppData,
    MissingConfigFile { path: PathBuf },
    ReadConfigFailed { path: PathBuf, details: String },
    ParseConfigFailed { path: PathBuf, details: String },
    MissingFile { field: &'static str, path: PathBuf },
    MissingDirectory { field: &'static str, path: PathBuf },
    MissingExecutable { path: PathBuf },
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAppData => {
                write!(
                    formatter,
                    "APPDATA is missing; cannot resolve %APPDATA%\\VoxGolem\\config.toml"
                )
            }
            Self::MissingConfigFile { path } => {
                write!(formatter, "config file not found: {}", path.display())
            }
            Self::ReadConfigFailed { path, details } => {
                write!(
                    formatter,
                    "failed to read config file {}: {details}",
                    path.display()
                )
            }
            Self::ParseConfigFailed { path, details } => {
                write!(
                    formatter,
                    "failed to parse config file {}: {details}",
                    path.display()
                )
            }
            Self::MissingFile { field, path } => {
                write!(
                    formatter,
                    "invalid `{field}` path; expected an existing file: {}",
                    path.display()
                )
            }
            Self::MissingDirectory { field, path } => {
                write!(
                    formatter,
                    "invalid `{field}` path; expected an existing directory: {}",
                    path.display()
                )
            }
            Self::MissingExecutable { path } => {
                write!(
                    formatter,
                    "invalid `opencode_path`; expected an existing executable file: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let app_data = std::env::var_os("APPDATA").ok_or(ConfigError::MissingAppData)?;

    Ok(PathBuf::from(app_data)
        .join(WINDOWS_CONFIG_DIR)
        .join(WINDOWS_CONFIG_FILE))
}

pub fn load_runtime_config(path_override: Option<&Path>) -> Result<RuntimeConfig, ConfigError> {
    let config_path = match path_override {
        Some(path) => path.to_path_buf(),
        None => default_config_path()?,
    };
    let config_dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let config_contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(ConfigError::MissingConfigFile { path: config_path });
        }
        Err(error) => {
            return Err(ConfigError::ReadConfigFailed {
                path: config_path,
                details: error.to_string(),
            });
        }
    };

    let raw_config = toml::from_str::<RawConfig>(&config_contents).map_err(|error| {
        ConfigError::ParseConfigFailed {
            path: config_path,
            details: error.to_string(),
        }
    })?;

    let wake_word_dir = resolve_config_path(&config_dir, raw_config.wake_word_dir);
    let parakeet_model_dir = resolve_config_path(&config_dir, raw_config.parakeet_model_dir);
    let silero_vad_model = resolve_config_path(
        &config_dir,
        raw_config
            .silero_vad_model
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SILERO_VAD_MODEL)),
    );
    let opencode_path = resolve_config_path(&config_dir, raw_config.opencode_path);

    validate_existing_directory(&wake_word_dir, "wake_word_dir")?;
    validate_existing_directory(&parakeet_model_dir, "parakeet_model_dir")?;
    validate_existing_file(&silero_vad_model, "silero_vad_model")?;
    validate_existing_executable(&opencode_path)?;

    Ok(RuntimeConfig {
        wake_word_dir,
        parakeet_model_dir,
        silero_vad_model,
        opencode_path,
    })
}

fn resolve_config_path(config_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn validate_existing_file(path: &Path, field: &'static str) -> Result<(), ConfigError> {
    if path.is_file() {
        return Ok(());
    }

    Err(ConfigError::MissingFile {
        field,
        path: path.to_path_buf(),
    })
}

fn validate_existing_directory(path: &Path, field: &'static str) -> Result<(), ConfigError> {
    if path.is_dir() {
        return Ok(());
    }

    Err(ConfigError::MissingDirectory {
        field,
        path: path.to_path_buf(),
    })
}

fn validate_existing_executable(path: &Path) -> Result<(), ConfigError> {
    if path.is_file() {
        return Ok(());
    }

    Err(ConfigError::MissingExecutable {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::{load_runtime_config, ConfigError};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();

            let path = std::env::temp_dir().join(format!(
                "voxgolem-config-tests-{}-{stamp}",
                std::process::id()
            ));

            fs::create_dir_all(&path).expect("temporary test directory should be creatable");

            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn reports_missing_config_file() {
        let temp = TempDir::new();
        let missing_path = temp.path().join("missing.toml");

        let result = load_runtime_config(Some(&missing_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingConfigFile { path: missing_path })
        );
    }

    #[test]
    fn reports_parse_failure_for_invalid_toml_structure() {
        let temp = TempDir::new();
        let config_path = temp.path().join("config.toml");

        fs::write(&config_path, "wake_word_dir = [\"unexpected array\"]")
            .expect("invalid config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(matches!(result, Err(ConfigError::ParseConfigFailed { .. })));
    }

    #[test]
    fn reports_missing_required_wake_word_directory() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&silero_vad_model);

        let opencode_path = temp.path().join("opencode.exe");
        create_file(&opencode_path);

        let missing_wake_word_dir = temp.path().join("missing-wake-word");
        let config_path = temp.path().join("config.toml");

        fs::write(
            &config_path,
            render_config(
                &missing_wake_word_dir,
                &model_dir,
                &silero_vad_model,
                &opencode_path,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingDirectory {
                field: "wake_word_dir",
                path: missing_wake_word_dir,
            })
        );
    }

    #[test]
    fn defaults_missing_silero_vad_model_to_models_directory() {
        let temp = TempDir::new();
        let wake_word_dir = temp.path().join("wake-word");
        let model_dir = temp.path().join("models");
        let default_silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&wake_word_dir).expect("wake word directory fixture should be created");
        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&default_silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_dir = \"{}\"\nparakeet_model_dir = \"{}\"\nopencode_path = \"{}\"\n",
                escape_path(&wake_word_dir),
                escape_path(&model_dir),
                escape_path(&opencode_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path))
            .expect("config without silero_vad_model should use default path");

        assert_eq!(result.silero_vad_model, default_silero_vad_model);
    }

    #[test]
    fn reports_missing_required_silero_vad_model_file() {
        let temp = TempDir::new();
        let wake_word_dir = temp.path().join("wake-word");
        let model_dir = temp.path().join("models");
        let missing_silero_vad_model = model_dir.join("missing-silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&wake_word_dir).expect("wake word directory fixture should be created");
        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&opencode_path);

        fs::write(
            &config_path,
            render_config(
                &wake_word_dir,
                &model_dir,
                &missing_silero_vad_model,
                &opencode_path,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingFile {
                field: "silero_vad_model",
                path: missing_silero_vad_model,
            })
        );
    }

    #[test]
    fn loads_valid_config() {
        let temp = TempDir::new();
        let wake_word_dir = temp.path().join("wake-word");
        let model_dir = temp.path().join("models");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&wake_word_dir).expect("wake word directory fixture should be created");
        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            render_config(
                &wake_word_dir,
                &model_dir,
                &silero_vad_model,
                &opencode_path,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result.expect("valid config should load").silero_vad_model,
            silero_vad_model
        );
    }

    fn create_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }

        fs::write(path, b"fixture").expect("file fixture should be written");
    }

    fn render_config(
        wake_word_dir: &Path,
        parakeet_model_dir: &Path,
        silero_vad_model: &Path,
        opencode_path: &Path,
    ) -> String {
        let silero_vad_model_line =
            format!("silero_vad_model = \"{}\"\n", escape_path(silero_vad_model));

        format!(
            "wake_word_dir = \"{}\"\nparakeet_model_dir = \"{}\"\n{}opencode_path = \"{}\"\n",
            escape_path(wake_word_dir),
            escape_path(parakeet_model_dir),
            silero_vad_model_line,
            escape_path(opencode_path),
        )
    }

    fn escape_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }
}
