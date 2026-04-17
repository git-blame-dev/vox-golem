use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const WINDOWS_CONFIG_DIR: &str = "VoxGolem";
const WINDOWS_CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    wake_word_wav: PathBuf,
    parakeet_model_dir: PathBuf,
    opencode_path: PathBuf,
    start_listening_cue: PathBuf,
    stop_listening_cue: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub wake_word_wav: PathBuf,
    pub parakeet_model_dir: PathBuf,
    pub opencode_path: PathBuf,
    pub start_listening_cue: PathBuf,
    pub stop_listening_cue: PathBuf,
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
                write!(formatter, "APPDATA is missing; cannot resolve %APPDATA%\\VoxGolem\\config.toml")
            }
            Self::MissingConfigFile { path } => {
                write!(formatter, "config file not found: {}", path.display())
            }
            Self::ReadConfigFailed { path, details } => {
                write!(formatter, "failed to read config file {}: {details}", path.display())
            }
            Self::ParseConfigFailed { path, details } => {
                write!(formatter, "failed to parse config file {}: {details}", path.display())
            }
            Self::MissingFile { field, path } => {
                write!(formatter, "invalid `{field}` path; expected an existing file: {}", path.display())
            }
            Self::MissingDirectory { field, path } => {
                write!(formatter, "invalid `{field}` path; expected an existing directory: {}", path.display())
            }
            Self::MissingExecutable { path } => {
                write!(formatter, "invalid `opencode_path`; expected an existing executable file: {}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    let app_data =
        std::env::var_os("APPDATA").ok_or(ConfigError::MissingAppData)?;

    Ok(PathBuf::from(app_data)
        .join(WINDOWS_CONFIG_DIR)
        .join(WINDOWS_CONFIG_FILE))
}

pub fn load_runtime_config(path_override: Option<&Path>) -> Result<RuntimeConfig, ConfigError> {
    let config_path = match path_override {
        Some(path) => path.to_path_buf(),
        None => default_config_path()?,
    };

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

    validate_existing_file(&raw_config.wake_word_wav, "wake_word_wav")?;
    validate_existing_directory(&raw_config.parakeet_model_dir, "parakeet_model_dir")?;
    validate_existing_file(&raw_config.start_listening_cue, "start_listening_cue")?;
    validate_existing_file(&raw_config.stop_listening_cue, "stop_listening_cue")?;
    validate_existing_executable(&raw_config.opencode_path)?;

    Ok(RuntimeConfig {
        wake_word_wav: raw_config.wake_word_wav,
        parakeet_model_dir: raw_config.parakeet_model_dir,
        opencode_path: raw_config.opencode_path,
        start_listening_cue: raw_config.start_listening_cue,
        stop_listening_cue: raw_config.stop_listening_cue,
    })
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

            fs::create_dir_all(&path)
                .expect("temporary test directory should be creatable");

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

        fs::write(
            &config_path,
            "wake_word_wav = [\"unexpected array\"]",
        )
        .expect("invalid config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(matches!(
            result,
            Err(ConfigError::ParseConfigFailed { .. })
        ));
    }

    #[test]
    fn reports_missing_required_wake_word_file() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        fs::create_dir_all(&model_dir)
            .expect("model directory fixture should be created");

        let opencode_path = temp.path().join("opencode.exe");
        create_file(&opencode_path);

        let start_cue = temp.path().join("assets/start-listening.mp3");
        let stop_cue = temp.path().join("assets/stop-listening.mp3");
        create_file(&start_cue);
        create_file(&stop_cue);

        let missing_wake_word = temp.path().join("missing-wake-word.wav");
        let config_path = temp.path().join("config.toml");

        fs::write(
            &config_path,
            render_config(
                &missing_wake_word,
                &model_dir,
                &opencode_path,
                &start_cue,
                &stop_cue,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingFile {
                field: "wake_word_wav",
                path: missing_wake_word,
            })
        );
    }

    #[test]
    fn loads_valid_config() {
        let temp = TempDir::new();
        let wake_word = temp.path().join("wake-word.wav");
        let model_dir = temp.path().join("models");
        let opencode_path = temp.path().join("opencode.exe");
        let start_cue = temp.path().join("assets/start-listening.mp3");
        let stop_cue = temp.path().join("assets/stop-listening.mp3");
        let config_path = temp.path().join("config.toml");

        create_file(&wake_word);
        fs::create_dir_all(&model_dir)
            .expect("model directory fixture should be created");
        create_file(&opencode_path);
        create_file(&start_cue);
        create_file(&stop_cue);

        fs::write(
            &config_path,
            render_config(
                &wake_word,
                &model_dir,
                &opencode_path,
                &start_cue,
                &stop_cue,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(result.is_ok());
    }

    fn create_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .expect("parent directory should be created");
        }

        fs::write(path, b"fixture")
            .expect("file fixture should be written");
    }

    fn render_config(
        wake_word_wav: &Path,
        parakeet_model_dir: &Path,
        opencode_path: &Path,
        start_listening_cue: &Path,
        stop_listening_cue: &Path,
    ) -> String {
        format!(
            "wake_word_wav = \"{}\"\nparakeet_model_dir = \"{}\"\nopencode_path = \"{}\"\nstart_listening_cue = \"{}\"\nstop_listening_cue = \"{}\"\n",
            escape_path(wake_word_wav),
            escape_path(parakeet_model_dir),
            escape_path(opencode_path),
            escape_path(start_listening_cue),
            escape_path(stop_listening_cue),
        )
    }

    fn escape_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }
}
