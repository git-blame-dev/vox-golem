use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const WINDOWS_CONFIG_DIR: &str = "VoxGolem";
const WINDOWS_CONFIG_FILE: &str = "config.toml";
const WINDOWS_SOUL_FILE: &str = "SOUL.md";
const DEFAULT_SILERO_VAD_MODEL: &str = "models/silero-vad.onnx";
const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_WAKE_WORD_DETECTION_THRESHOLD: f32 = 0.68;
const DEFAULT_TTS_WORKER_COUNT: usize = 1;
const DEFAULT_TTS_MAX_QUEUE: usize = 8;
const DEFAULT_TTS_SAMPLE_RATE_HZ: u32 = 22_050;
const DEFAULT_TTS_MAX_DURATION_S: u64 = 300;
const DEFAULT_TTS_OUTPUT_GAIN_DB: f32 = 3.0;
const MIN_TTS_OUTPUT_GAIN_DB: f32 = -24.0;
const MAX_TTS_OUTPUT_GAIN_DB: f32 = 24.0;
const MAX_JS_SAFE_INTEGER_U64: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    wake_word_model_path: PathBuf,
    parakeet_model_dir: PathBuf,
    silero_vad_model: Option<PathBuf>,
    #[serde(default = "default_silence_timeout_ms")]
    silence_timeout_ms: u64,
    #[serde(default = "default_wake_word_detection_threshold")]
    wake_word_detection_threshold: f32,
    response_backend: RawResponseBackend,
    #[serde(default)]
    opencode: Option<RawOpencodeConfig>,
    #[serde(default)]
    llama_cpp: Option<RawLlamaCppConfig>,
    #[serde(default)]
    tts: Option<RawTtsConfig>,
    #[serde(default, rename = "start_listening_cue")]
    _start_listening_cue: Option<PathBuf>,
    #[serde(default, rename = "stop_listening_cue")]
    _stop_listening_cue: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RawResponseBackend {
    Opencode,
    LlamaCpp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOpencodeConfig {
    path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLlamaCppConfig {
    server_path: PathBuf,
    host: String,
    port: u16,
    fast_model_path: PathBuf,
    #[serde(default)]
    quality_model_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTtsConfig {
    #[serde(default)]
    enabled: bool,
    model_path: PathBuf,
    #[serde(default = "default_tts_worker_count")]
    worker_count: usize,
    #[serde(default = "default_tts_max_queue")]
    max_queue: usize,
    #[serde(default = "default_tts_sample_rate_hz")]
    sample_rate_hz: u32,
    #[serde(default = "default_tts_max_duration_s")]
    max_duration_s: u64,
    #[serde(default = "default_tts_output_gain_db")]
    output_gain_db: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    pub wake_word_model_path: PathBuf,
    pub parakeet_model_dir: PathBuf,
    pub silero_vad_model: PathBuf,
    pub silence_timeout_ms: u64,
    pub wake_word_detection_threshold: f32,
    pub local_tts: LocalTtsConfig,
    pub response_backend: ResponseBackendConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalTtsConfig {
    pub enabled: bool,
    pub model_path: PathBuf,
    pub worker_count: usize,
    pub max_queue: usize,
    pub sample_rate_hz: u32,
    pub max_duration_s: u64,
    pub output_gain_db: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseBackendConfig {
    Opencode {
        path: PathBuf,
    },
    LlamaCpp {
        server_path: PathBuf,
        host: String,
        port: u16,
        fast_model_path: PathBuf,
        quality_model_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingAppData,
    MissingConfigFile { path: PathBuf },
    ReadConfigFailed { path: PathBuf, details: String },
    ParseConfigFailed { path: PathBuf, details: String },
    MissingFile { field: &'static str, path: PathBuf },
    MissingDirectory { field: &'static str, path: PathBuf },
    MissingExecutable { field: &'static str, path: PathBuf },
    MissingBackendConfig { backend: &'static str },
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
            Self::MissingExecutable { field, path } => {
                write!(
                    formatter,
                    "invalid `{field}` path; expected an existing executable file: {}",
                    path.display()
                )
            }
            Self::MissingBackendConfig { backend } => {
                write!(
                    formatter,
                    "missing configuration table for selected backend `{backend}`"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    Ok(default_app_data_dir()?.join(WINDOWS_CONFIG_FILE))
}

pub fn default_soul_path() -> Result<PathBuf, ConfigError> {
    Ok(default_app_data_dir()?.join(WINDOWS_SOUL_FILE))
}

fn default_app_data_dir() -> Result<PathBuf, ConfigError> {
    let app_data = std::env::var_os("APPDATA").ok_or(ConfigError::MissingAppData)?;

    Ok(PathBuf::from(app_data).join(WINDOWS_CONFIG_DIR))
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
            path: config_path.clone(),
            details: error.to_string(),
        }
    })?;

    let wake_word_model_path = resolve_config_path(&config_dir, raw_config.wake_word_model_path);
    let parakeet_model_dir = resolve_config_path(&config_dir, raw_config.parakeet_model_dir);
    let silero_vad_model = resolve_config_path(
        &config_dir,
        raw_config
            .silero_vad_model
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SILERO_VAD_MODEL)),
    );
    let silence_timeout_ms = raw_config.silence_timeout_ms;
    let wake_word_detection_threshold = raw_config.wake_word_detection_threshold;

    if silence_timeout_ms == 0 {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from("silence_timeout_ms must be greater than zero"),
        });
    }

    if silence_timeout_ms > MAX_JS_SAFE_INTEGER_U64 {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from(
                "silence_timeout_ms must be less than or equal to 9_007_199_254_740_991",
            ),
        });
    }

    if !wake_word_detection_threshold.is_finite() {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from("wake_word_detection_threshold must be a finite number"),
        });
    }

    if !(0.0..=1.0).contains(&wake_word_detection_threshold) {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from(
                "wake_word_detection_threshold must be between 0.0 and 1.0 inclusive",
            ),
        });
    }

    validate_existing_file(&wake_word_model_path, "wake_word_model_path")?;
    validate_existing_directory(&parakeet_model_dir, "parakeet_model_dir")?;
    validate_existing_file(&silero_vad_model, "silero_vad_model")?;

    let local_tts = match raw_config.tts {
        Some(raw_tts) => {
            let model_path = resolve_config_path(&config_dir, raw_tts.model_path);
            validate_existing_file(&model_path, "tts.model_path")?;

            if raw_tts.worker_count == 0 {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("tts.worker_count must be greater than zero"),
                });
            }

            if raw_tts.max_queue == 0 {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("tts.max_queue must be greater than zero"),
                });
            }

            if raw_tts.sample_rate_hz == 0 {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("tts.sample_rate_hz must be greater than zero"),
                });
            }

            if raw_tts.max_duration_s == 0 {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("tts.max_duration_s must be greater than zero"),
                });
            }

            if !raw_tts.output_gain_db.is_finite() {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("tts.output_gain_db must be a finite number"),
                });
            }

            if !(MIN_TTS_OUTPUT_GAIN_DB..=MAX_TTS_OUTPUT_GAIN_DB).contains(&raw_tts.output_gain_db)
            {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: format!(
                        "tts.output_gain_db must be between {MIN_TTS_OUTPUT_GAIN_DB} and {MAX_TTS_OUTPUT_GAIN_DB} inclusive"
                    ),
                });
            }

            LocalTtsConfig {
                enabled: raw_tts.enabled,
                model_path,
                worker_count: raw_tts.worker_count,
                max_queue: raw_tts.max_queue,
                sample_rate_hz: raw_tts.sample_rate_hz,
                max_duration_s: raw_tts.max_duration_s,
                output_gain_db: raw_tts.output_gain_db,
            }
        }
        None => {
            let model_path =
                resolve_config_path(&config_dir, PathBuf::from("models/tts/jarvis.onnx"));

            LocalTtsConfig {
                enabled: false,
                model_path,
                worker_count: DEFAULT_TTS_WORKER_COUNT,
                max_queue: DEFAULT_TTS_MAX_QUEUE,
                sample_rate_hz: DEFAULT_TTS_SAMPLE_RATE_HZ,
                max_duration_s: DEFAULT_TTS_MAX_DURATION_S,
                output_gain_db: DEFAULT_TTS_OUTPUT_GAIN_DB,
            }
        }
    };

    let response_backend = match raw_config.response_backend {
        RawResponseBackend::Opencode => {
            let raw_opencode = raw_config
                .opencode
                .ok_or(ConfigError::MissingBackendConfig {
                    backend: "opencode",
                })?;
            let path = resolve_config_path(&config_dir, raw_opencode.path);
            validate_existing_executable(&path, "opencode.path")?;

            ResponseBackendConfig::Opencode { path }
        }
        RawResponseBackend::LlamaCpp => {
            let raw_llama_cpp = raw_config
                .llama_cpp
                .ok_or(ConfigError::MissingBackendConfig {
                    backend: "llama_cpp",
                })?;
            let server_path = resolve_config_path(&config_dir, raw_llama_cpp.server_path);
            let host = raw_llama_cpp.host.trim().to_string();
            let port = raw_llama_cpp.port;
            let fast_model_path = resolve_config_path(&config_dir, raw_llama_cpp.fast_model_path);
            let quality_model_path = raw_llama_cpp
                .quality_model_path
                .map(|path| resolve_config_path(&config_dir, path));

            if host.is_empty() {
                return Err(ConfigError::ParseConfigFailed {
                    path: config_path.clone(),
                    details: String::from("llama_cpp.host must not be empty"),
                });
            }

            validate_existing_executable(&server_path, "llama_cpp.server_path")?;
            validate_existing_file(&fast_model_path, "llama_cpp.fast_model_path")?;

            ResponseBackendConfig::LlamaCpp {
                server_path,
                host,
                port,
                fast_model_path,
                quality_model_path,
            }
        }
    };

    Ok(RuntimeConfig {
        wake_word_model_path,
        parakeet_model_dir,
        silero_vad_model,
        silence_timeout_ms,
        wake_word_detection_threshold,
        local_tts,
        response_backend,
    })
}

fn default_silence_timeout_ms() -> u64 {
    DEFAULT_SILENCE_TIMEOUT_MS
}

fn default_wake_word_detection_threshold() -> f32 {
    DEFAULT_WAKE_WORD_DETECTION_THRESHOLD
}

fn default_tts_worker_count() -> usize {
    DEFAULT_TTS_WORKER_COUNT
}

fn default_tts_max_queue() -> usize {
    DEFAULT_TTS_MAX_QUEUE
}

fn default_tts_sample_rate_hz() -> u32 {
    DEFAULT_TTS_SAMPLE_RATE_HZ
}

fn default_tts_max_duration_s() -> u64 {
    DEFAULT_TTS_MAX_DURATION_S
}

fn default_tts_output_gain_db() -> f32 {
    DEFAULT_TTS_OUTPUT_GAIN_DB
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

fn validate_existing_executable(path: &Path, field: &'static str) -> Result<(), ConfigError> {
    if path.is_file() {
        return Ok(());
    }

    Err(ConfigError::MissingExecutable {
        field,
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::{load_runtime_config, ConfigError, ResponseBackendConfig};
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

        fs::write(
            &config_path,
            "wake_word_model_path = [\"unexpected array\"]",
        )
        .expect("invalid config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(matches!(result, Err(ConfigError::ParseConfigFailed { .. })));
    }

    #[test]
    fn reports_missing_required_wake_word_model_file() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");
        let missing_wake_word_model_path = model_dir.join("hey_livekit.onnx");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            render_opencode_config(
                &missing_wake_word_model_path,
                &model_dir,
                &silero_vad_model,
                &opencode_path,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingFile {
                field: "wake_word_model_path",
                path: missing_wake_word_model_path,
            })
        );
    }

    #[test]
    fn defaults_missing_silero_vad_model_to_models_directory() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let default_silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&default_silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&opencode_path),
            ),
        )
        .expect("config without silero_vad_model should be written");

        let result = load_runtime_config(Some(&config_path))
            .expect("config without silero_vad_model should use default path");

        assert_eq!(result.silero_vad_model, default_silero_vad_model);
        assert_eq!(result.silence_timeout_ms, 1_500);
    }

    #[test]
    fn reports_parse_failure_for_zero_silence_timeout() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nsilence_timeout_ms = 0\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(matches!(
            result,
            Err(ConfigError::ParseConfigFailed { details, .. })
                if details.contains("silence_timeout_ms must be greater than zero")
        ));
    }

    #[test]
    fn loads_custom_silence_timeout() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nsilence_timeout_ms = 1750\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path)).expect("valid config should load");

        assert_eq!(result.silence_timeout_ms, 1_750);
    }

    #[test]
    fn reports_parse_failure_for_silence_timeout_above_js_safe_integer() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nsilence_timeout_ms = 9007199254740992\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert!(matches!(
            result,
            Err(ConfigError::ParseConfigFailed { details, .. })
                if details.contains("silence_timeout_ms must be less than or equal to 9_007_199_254_740_991")
        ));
    }

    #[test]
    fn reports_missing_required_silero_vad_model_file() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let missing_silero_vad_model = model_dir.join("missing-silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            render_opencode_config(
                &wake_word_model_path,
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
    fn reports_missing_backend_table_for_selected_backend() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"llama_cpp\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingBackendConfig {
                backend: "llama_cpp",
            })
        );
    }

    #[test]
    fn reports_missing_opencode_table_for_selected_backend() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingBackendConfig {
                backend: "opencode",
            })
        );
    }

    #[test]
    fn loads_valid_opencode_backend_config() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            render_opencode_config(
                &wake_word_model_path,
                &model_dir,
                &silero_vad_model,
                &opencode_path,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path)).expect("valid config should load");

        assert_eq!(
            result.response_backend,
            ResponseBackendConfig::Opencode {
                path: opencode_path,
            }
        );
    }

    #[test]
    fn reports_missing_required_tts_model_file() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let missing_tts_model_path = model_dir.join("tts").join("jarvis.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n\n[tts]\nenabled = true\nmodel_path = \"{}\"\nworker_count = 1\nmax_queue = 8\nsample_rate_hz = 22050\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
                escape_path(&missing_tts_model_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::MissingFile {
                field: "tts.model_path",
                path: missing_tts_model_path,
            })
        );
    }

    #[test]
    fn loads_valid_tts_config_when_present() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let tts_model_path = model_dir.join("tts").join("jarvis.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&tts_model_path);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n\n[tts]\nenabled = true\nmodel_path = \"{}\"\nworker_count = 2\nmax_queue = 16\nsample_rate_hz = 24000\nmax_duration_s = 360\noutput_gain_db = 6.0\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
                escape_path(&tts_model_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path)).expect("valid config should load");

        assert!(result.local_tts.enabled);
        assert_eq!(result.local_tts.model_path, tts_model_path);
        assert_eq!(result.local_tts.worker_count, 2);
        assert_eq!(result.local_tts.max_queue, 16);
        assert_eq!(result.local_tts.sample_rate_hz, 24_000);
        assert_eq!(result.local_tts.max_duration_s, 360);
        assert_eq!(result.local_tts.output_gain_db, 6.0);
    }

    #[test]
    fn reports_invalid_tts_max_duration_when_zero() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let tts_model_path = model_dir.join("tts").join("jarvis.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&tts_model_path);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n\n[tts]\nenabled = true\nmodel_path = \"{}\"\nworker_count = 2\nmax_queue = 16\nsample_rate_hz = 24000\nmax_duration_s = 0\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
                escape_path(&tts_model_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::ParseConfigFailed {
                path: config_path,
                details: String::from("tts.max_duration_s must be greater than zero"),
            })
        );
    }

    #[test]
    fn reports_invalid_tts_output_gain_db_when_out_of_range() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let tts_model_path = model_dir.join("tts").join("jarvis.onnx");
        let opencode_path = temp.path().join("opencode.exe");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&tts_model_path);
        create_file(&opencode_path);

        fs::write(
            &config_path,
            format!(
                "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n\n[tts]\nenabled = true\nmodel_path = \"{}\"\nworker_count = 2\nmax_queue = 16\nsample_rate_hz = 24000\nmax_duration_s = 360\noutput_gain_db = 100.0\n",
                escape_path(&wake_word_model_path),
                escape_path(&model_dir),
                escape_path(&silero_vad_model),
                escape_path(&opencode_path),
                escape_path(&tts_model_path),
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path));

        assert_eq!(
            result,
            Err(ConfigError::ParseConfigFailed {
                path: config_path,
                details: String::from("tts.output_gain_db must be between -24 and 24 inclusive",),
            })
        );
    }

    #[test]
    fn loads_valid_llama_cpp_backend_config() {
        let temp = TempDir::new();
        let model_dir = temp.path().join("models");
        let wake_word_model_path = model_dir.join("hey_livekit.onnx");
        let silero_vad_model = model_dir.join("silero-vad.onnx");
        let llama_dir = temp.path().join("llama");
        let server_path = llama_dir.join("llama-server.exe");
        let fast_model_path = model_dir.join("gemma-3-1b-it-Q4_K_M.gguf");
        let config_path = temp.path().join("config.toml");

        fs::create_dir_all(&model_dir).expect("model directory fixture should be created");
        fs::create_dir_all(&llama_dir).expect("llama directory fixture should be created");
        create_file(&wake_word_model_path);
        create_file(&silero_vad_model);
        create_file(&server_path);
        create_file(&fast_model_path);

        fs::write(
            &config_path,
            render_llama_cpp_config(
                &wake_word_model_path,
                &model_dir,
                &silero_vad_model,
                &server_path,
                &fast_model_path,
                None,
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path)).expect("valid config should load");

        assert_eq!(
            result.response_backend,
            ResponseBackendConfig::LlamaCpp {
                server_path,
                host: String::from("127.0.0.1"),
                port: 11_435,
                fast_model_path,
                quality_model_path: None,
            }
        );
    }

    fn create_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory should be created");
        }

        fs::write(path, b"fixture").expect("file fixture should be written");
    }

    fn render_opencode_config(
        wake_word_model_path: &Path,
        parakeet_model_dir: &Path,
        silero_vad_model: &Path,
        opencode_path: &Path,
    ) -> String {
        format!(
            "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"opencode\"\n\n[opencode]\npath = \"{}\"\n",
            escape_path(wake_word_model_path),
            escape_path(parakeet_model_dir),
            escape_path(silero_vad_model),
            escape_path(opencode_path),
        )
    }

    fn render_llama_cpp_config(
        wake_word_model_path: &Path,
        parakeet_model_dir: &Path,
        silero_vad_model: &Path,
        server_path: &Path,
        fast_model_path: &Path,
        quality_model_path: Option<&Path>,
    ) -> String {
        let quality_model_line = quality_model_path.map_or(String::new(), |path| {
            format!("quality_model_path = \"{}\"\n", escape_path(path))
        });

        format!(
            "wake_word_model_path = \"{}\"\nparakeet_model_dir = \"{}\"\nsilero_vad_model = \"{}\"\nresponse_backend = \"llama_cpp\"\n\n[llama_cpp]\nserver_path = \"{}\"\nhost = \"127.0.0.1\"\nport = 11435\nfast_model_path = \"{}\"\n{}",
            escape_path(wake_word_model_path),
            escape_path(parakeet_model_dir),
            escape_path(silero_vad_model),
            escape_path(server_path),
            escape_path(fast_model_path),
            quality_model_line,
        )
    }

    fn escape_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }
}
