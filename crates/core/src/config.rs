use serde::Deserialize;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const WINDOWS_CONFIG_DIR: &str = "VoxGolem";
const WINDOWS_CONFIG_FILE: &str = "config.toml";
const WINDOWS_SOUL_FILE: &str = "SOUL.md";
const APP_DIR: &str = "voxgolem";
const DEFAULT_SILERO_VAD_MODEL: &str = "models/silero-vad.onnx";
const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_WAKE_WORD_DETECTION_THRESHOLD: f32 = 0.68;
const DEFAULT_TTS_WORKER_COUNT: usize = 1;
const DEFAULT_TTS_MAX_QUEUE: usize = 8;
const DEFAULT_TTS_SAMPLE_RATE_HZ: u32 = 22_050;
const DEFAULT_TTS_MAX_DURATION_S: u64 = 300;
const DEFAULT_TTS_OUTPUT_GAIN_DB: f32 = 3.0;
const DEFAULT_TELEMETRY_MAX_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_TELEMETRY_BACKUP_COUNT: u8 = 3;
const MIN_TELEMETRY_MAX_BYTES: usize = 1024;
const MAX_TELEMETRY_BACKUP_COUNT: u8 = 10;
const MIN_TTS_OUTPUT_GAIN_DB: f32 = -24.0;
const MAX_TTS_OUTPUT_GAIN_DB: f32 = 24.0;
const MAX_JS_SAFE_INTEGER_U64: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    wake_word_model_path: Option<PathBuf>,
    #[serde(default)]
    parakeet_model_dir: Option<PathBuf>,
    silero_vad_model: Option<PathBuf>,
    #[serde(default = "default_silence_timeout_ms")]
    silence_timeout_ms: u64,
    #[serde(default = "default_wake_word_detection_threshold")]
    wake_word_detection_threshold: f32,
    #[serde(default)]
    response_backend: Option<RawResponseBackend>,
    #[serde(default)]
    opencode: Option<RawOpencodeConfig>,
    #[serde(default)]
    llama_cpp: Option<RawLlamaCppConfig>,
    #[serde(default)]
    custom_openai: Option<RawCustomOpenAiConfig>,
    #[serde(default)]
    completion: Option<RawCompletionConfig>,
    #[serde(default)]
    tts: Option<RawTtsConfig>,
    #[serde(default)]
    logging: Option<RawLoggingConfig>,
    #[serde(default)]
    telemetry: Option<RawTelemetryConfig>,
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
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    runtime: OpencodeRuntime,
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
    #[serde(default)]
    inference_provider: InferencePolicy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCustomOpenAiConfig {
    #[serde(default = "default_private_endpoint")]
    endpoint: String,
    #[serde(default)]
    auth_path: Option<PathBuf>,
    #[serde(default)]
    auth_source: CustomOpenAiAuthSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCompletionConfig {
    server_path: PathBuf,
    model_path: PathBuf,
    #[serde(default)]
    inference_provider: InferencePolicy,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoggingConfig {
    #[serde(default)]
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTelemetryConfig {
    #[serde(default = "default_telemetry_enabled")]
    enabled: bool,
    #[serde(default = "default_telemetry_max_bytes")]
    max_bytes: usize,
    #[serde(default = "default_telemetry_backup_count")]
    backup_count: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    pub wake_word_model_path: PathBuf,
    pub parakeet_model_dir: PathBuf,
    pub silero_vad_model: PathBuf,
    pub silence_timeout_ms: u64,
    pub wake_word_detection_threshold: f32,
    pub local_tts: LocalTtsConfig,
    pub logging: LoggingConfig,
    pub telemetry: TelemetryConfig,
    pub response_backend: ResponseBackendConfig,
    pub opencode: Option<OpencodeConfig>,
    pub llama_cpp: Option<LlamaCppConfig>,
    pub custom_openai: Option<CustomOpenAiConfig>,
    pub completion: Option<CompletionConfig>,
    pub capability_issues: Vec<CapabilityConfigIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeConfig {
    pub path: PathBuf,
    pub runtime: OpencodeRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppConfig {
    pub server_path: PathBuf,
    pub host: String,
    pub port: u16,
    pub fast_model_path: PathBuf,
    pub quality_model_path: Option<PathBuf>,
    pub inference_provider: InferencePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomOpenAiConfig {
    pub endpoint: String,
    pub auth_path: PathBuf,
    pub auth_source: CustomOpenAiAuthSource,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CustomOpenAiAuthSource {
    #[default]
    Native,
    Wsl,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OpencodeRuntime {
    #[default]
    Native,
    Wsl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionConfig {
    pub server_path: PathBuf,
    pub model_path: PathBuf,
    pub inference_provider: InferencePolicy,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InferencePolicy {
    #[default]
    Auto,
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityConfigIssue {
    pub capability: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoggingConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub max_bytes: usize,
    pub backup_count: u8,
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
    Unconfigured,
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
    ReadConfigFailed { path: PathBuf, details: String },
    ParseConfigFailed { path: PathBuf, details: String },
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingAppData => {
                write!(formatter, "home directory is unavailable")
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
        }
    }
}

impl std::error::Error for ConfigError {}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("VOXGOLEM_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(default_config_dir()?.join(WINDOWS_CONFIG_FILE))
}

pub fn default_soul_path() -> Result<PathBuf, ConfigError> {
    Ok(soul_path_for_config(default_config_path()?))
}

fn soul_path_for_config(config_path: PathBuf) -> PathBuf {
    config_path.with_file_name(WINDOWS_SOUL_FILE)
}

pub fn default_data_dir() -> Result<PathBuf, ConfigError> {
    if cfg!(windows) {
        return Ok(platform_dirs_from_env(|name| std::env::var_os(name), true)?.data);
    }
    linux_app_dir_from_env(
        &mut |name| std::env::var_os(name),
        "XDG_DATA_HOME",
        ".local/share",
    )
}

pub fn default_state_dir() -> Result<PathBuf, ConfigError> {
    if cfg!(windows) {
        return Ok(platform_dirs_from_env(|name| std::env::var_os(name), true)?.state);
    }
    linux_app_dir_from_env(
        &mut |name| std::env::var_os(name),
        "XDG_STATE_HOME",
        ".local/state",
    )
}

pub fn default_cache_dir() -> Result<PathBuf, ConfigError> {
    if cfg!(windows) {
        return Ok(platform_dirs_from_env(|name| std::env::var_os(name), true)?.cache);
    }
    linux_app_dir_from_env(
        &mut |name| std::env::var_os(name),
        "XDG_CACHE_HOME",
        ".cache",
    )
}

fn default_config_dir() -> Result<PathBuf, ConfigError> {
    if cfg!(windows) {
        return Ok(platform_dirs_from_env(|name| std::env::var_os(name), true)?.config);
    }
    linux_app_dir_from_env(
        &mut |name| std::env::var_os(name),
        "XDG_CONFIG_HOME",
        ".config",
    )
}

#[derive(Debug, PartialEq, Eq)]
struct PlatformDirs {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

fn platform_dirs_from_env<F>(mut get: F, windows: bool) -> Result<PlatformDirs, ConfigError>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    if windows {
        let profile = get("USERPROFILE");
        let base = get("APPDATA")
            .or_else(|| {
                profile
                    .clone()
                    .map(|p| PathBuf::from(p).join("AppData/Roaming").into_os_string())
            })
            .ok_or(ConfigError::MissingAppData)?;
        let local = get("LOCALAPPDATA")
            .or_else(|| profile.map(|p| PathBuf::from(p).join("AppData/Local").into_os_string()))
            .unwrap_or_else(|| base.clone());
        let config = PathBuf::from(base).join(WINDOWS_CONFIG_DIR);
        let local = PathBuf::from(local).join(WINDOWS_CONFIG_DIR);
        return Ok(PlatformDirs {
            config,
            data: local.clone(),
            state: local.clone(),
            cache: local,
        });
    }

    Ok(PlatformDirs {
        config: linux_app_dir_from_env(&mut get, "XDG_CONFIG_HOME", ".config")?,
        data: linux_app_dir_from_env(&mut get, "XDG_DATA_HOME", ".local/share")?,
        state: linux_app_dir_from_env(&mut get, "XDG_STATE_HOME", ".local/state")?,
        cache: linux_app_dir_from_env(&mut get, "XDG_CACHE_HOME", ".cache")?,
    })
}

fn linux_app_dir_from_env<F>(
    get: &mut F,
    xdg_name: &str,
    home_fallback: &str,
) -> Result<PathBuf, ConfigError>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    get(xdg_name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| get("HOME").map(|home| PathBuf::from(home).join(home_fallback)))
        .map(|base| base.join(APP_DIR))
        .ok_or(ConfigError::MissingAppData)
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
            return Ok(default_runtime_config(
                &config_dir,
                Some(format!("config file not found: {}", config_path.display())),
            ));
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

    if raw_config
        .opencode
        .as_ref()
        .is_some_and(|raw| raw.runtime == OpencodeRuntime::Native && raw.path.is_none())
    {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from("opencode.path is required for native runtime"),
        });
    }
    if let Some(path) = raw_config
        .opencode
        .as_ref()
        .filter(|raw| raw.runtime == OpencodeRuntime::Wsl)
        .and_then(|raw| raw.path.as_ref())
        .filter(|path| !is_absolute_linux_path(path))
    {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: format!(
                "opencode.path must be an absolute Linux path for WSL runtime: {}",
                path.display()
            ),
        });
    }
    if let Some(path) = raw_config
        .custom_openai
        .as_ref()
        .filter(|raw| raw.auth_source == CustomOpenAiAuthSource::Wsl)
        .and_then(|raw| raw.auth_path.as_ref())
        .filter(|path| !is_absolute_linux_path(path))
    {
        return Err(ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: format!(
                "custom_openai.auth_path must be an absolute Linux path for WSL auth: {}",
                path.display()
            ),
        });
    }

    let wake_word_configured = raw_config.wake_word_model_path.is_some();
    let parakeet_configured = raw_config.parakeet_model_dir.is_some();
    let vad_configured = raw_config.silero_vad_model.is_some();
    let wake_word_model_path = resolve_config_path(
        &config_dir,
        raw_config
            .wake_word_model_path
            .unwrap_or_else(|| PathBuf::from("models/hey_livekit.onnx")),
    );
    let parakeet_model_dir = resolve_config_path(
        &config_dir,
        raw_config
            .parakeet_model_dir
            .unwrap_or_else(|| PathBuf::from("models/parakeet-v2")),
    );
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

    let mut capability_issues = Vec::new();
    record_path_issue(
        &mut capability_issues,
        "wake_word",
        wake_word_configured,
        wake_word_model_path.is_file(),
        &wake_word_model_path,
    );
    record_path_issue(
        &mut capability_issues,
        "parakeet",
        parakeet_configured,
        parakeet_model_dir.is_dir(),
        &parakeet_model_dir,
    );
    record_path_issue(
        &mut capability_issues,
        "vad",
        vad_configured,
        silero_vad_model.is_file(),
        &silero_vad_model,
    );

    let local_tts = match raw_config.tts {
        Some(raw_tts) => {
            let model_path = resolve_config_path(&config_dir, raw_tts.model_path);
            record_path_issue(
                &mut capability_issues,
                "tts",
                true,
                model_path.is_file(),
                &model_path,
            );

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

    let logging = LoggingConfig {
        enabled: raw_config
            .logging
            .map(|raw_logging| raw_logging.enabled)
            .unwrap_or(false),
    };

    let telemetry = raw_config
        .telemetry
        .map(|raw| {
            if raw.max_bytes < MIN_TELEMETRY_MAX_BYTES {
                return Err("telemetry.max_bytes must be at least 1024");
            }
            if raw.backup_count > MAX_TELEMETRY_BACKUP_COUNT {
                return Err("telemetry.backup_count must be between 0 and 10 inclusive");
            }
            Ok(TelemetryConfig {
                enabled: raw.enabled,
                max_bytes: raw.max_bytes,
                backup_count: raw.backup_count,
            })
        })
        .transpose()
        .map_err(|details| ConfigError::ParseConfigFailed {
            path: config_path.clone(),
            details: String::from(details),
        })?
        .unwrap_or(TelemetryConfig {
            enabled: true,
            max_bytes: DEFAULT_TELEMETRY_MAX_BYTES,
            backup_count: DEFAULT_TELEMETRY_BACKUP_COUNT,
        });

    if let Some(raw_llama_cpp) = raw_config.llama_cpp.as_ref() {
        let host = raw_llama_cpp.host.trim();
        if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
            return Err(ConfigError::ParseConfigFailed {
                path: config_path.clone(),
                details: String::from(
                    "llama_cpp.host must be a supported loopback host (localhost, 127.0.0.1, or ::1)",
                ),
            });
        }
        if raw_llama_cpp.port == 0 {
            return Err(ConfigError::ParseConfigFailed {
                path: config_path.clone(),
                details: String::from("llama_cpp.port must be greater than zero"),
            });
        }
    }

    let opencode = raw_config.opencode.as_ref().map(|raw| {
        let path = match raw.runtime {
            OpencodeRuntime::Wsl => raw.path.clone().unwrap_or_default(),
            OpencodeRuntime::Native => raw
                .path
                .clone()
                .map_or_else(PathBuf::new, |path| resolve_config_path(&config_dir, path)),
        };
        match raw.runtime {
            OpencodeRuntime::Wsl if !cfg!(windows) => {
                capability_issues.push(CapabilityConfigIssue {
                    capability: "opencode",
                    reason: String::from(
                        "WSL runtime is only supported by the Windows application",
                    ),
                });
            }
            OpencodeRuntime::Wsl => {}
            OpencodeRuntime::Native => record_path_issue(
                &mut capability_issues,
                "opencode",
                true,
                path.is_file(),
                &path,
            ),
        }
        OpencodeConfig {
            path,
            runtime: raw.runtime,
        }
    });
    let llama_cpp = raw_config.llama_cpp.as_ref().map(|raw| {
        let server_path = resolve_config_path(&config_dir, raw.server_path.clone());
        let fast_model_path = resolve_config_path(&config_dir, raw.fast_model_path.clone());
        let quality_model_path = raw
            .quality_model_path
            .clone()
            .map(|p| resolve_config_path(&config_dir, p));
        let host = raw.host.trim().to_string();
        record_path_issue(
            &mut capability_issues,
            "local_fast",
            true,
            server_path.is_file() && fast_model_path.is_file(),
            if !server_path.is_file() {
                &server_path
            } else {
                &fast_model_path
            },
        );
        if let Some(path) = quality_model_path.as_ref() {
            let unavailable_path = if !server_path.is_file() {
                &server_path
            } else {
                path
            };
            record_path_issue(
                &mut capability_issues,
                "local_quality",
                true,
                server_path.is_file() && path.is_file(),
                unavailable_path,
            );
        }
        LlamaCppConfig {
            server_path,
            host,
            port: raw.port,
            fast_model_path,
            quality_model_path,
            inference_provider: raw.inference_provider,
        }
    });
    let custom_openai = raw_config.custom_openai.as_ref().map(|raw| {
        let auth_path = match raw.auth_source {
            CustomOpenAiAuthSource::Wsl => raw.auth_path.clone().unwrap_or_default(),
            CustomOpenAiAuthSource::Native => {
                let path = raw.auth_path.clone().unwrap_or_else(default_auth_path);
                if path.as_os_str().is_empty() {
                    path
                } else {
                    resolve_config_path(&config_dir, path)
                }
            }
        };
        let endpoint = raw.endpoint.trim().to_string();
        if !valid_custom_endpoint(&endpoint) {
            capability_issues.push(CapabilityConfigIssue {
                capability: "custom_provider",
                reason: String::from("custom_openai.endpoint is invalid"),
            });
        }
        record_custom_auth_issue(
            &mut capability_issues,
            raw.auth_source,
            &auth_path,
            cfg!(windows),
        );
        CustomOpenAiConfig {
            endpoint,
            auth_path,
            auth_source: raw.auth_source,
        }
    });
    let completion = raw_config.completion.as_ref().map(|raw| {
        let server_path = resolve_config_path(&config_dir, raw.server_path.clone());
        let model_path = resolve_config_path(&config_dir, raw.model_path.clone());
        record_path_issue(
            &mut capability_issues,
            "qwen_prediction",
            true,
            server_path.is_file() && model_path.is_file(),
            if !server_path.is_file() {
                &server_path
            } else {
                &model_path
            },
        );
        CompletionConfig {
            server_path,
            model_path,
            inference_provider: raw.inference_provider,
        }
    });

    let response_backend =
        match raw_config.response_backend {
            Some(RawResponseBackend::Opencode) => {
                let Some(_raw_opencode) = raw_config.opencode else {
                    capability_issues.push(CapabilityConfigIssue {
                        capability: "opencode",
                        reason: String::from("[opencode] table is not configured"),
                    });
                    return Ok(RuntimeConfig {
                        wake_word_model_path,
                        parakeet_model_dir,
                        silero_vad_model,
                        silence_timeout_ms,
                        wake_word_detection_threshold,
                        local_tts,
                        logging,
                        telemetry,
                        response_backend: ResponseBackendConfig::Unconfigured,
                        opencode: opencode.clone(),
                        llama_cpp: llama_cpp.clone(),
                        custom_openai: custom_openai.clone(),
                        completion: completion.clone(),
                        capability_issues,
                    });
                };
                if let Some(config) = opencode.as_ref().filter(|config| {
                    config.runtime == OpencodeRuntime::Wsl || config.path.is_file()
                }) {
                    ResponseBackendConfig::Opencode {
                        path: config.path.clone(),
                    }
                } else {
                    ResponseBackendConfig::Unconfigured
                }
            }
            Some(RawResponseBackend::LlamaCpp) => {
                let Some(_raw_llama_cpp) = raw_config.llama_cpp else {
                    capability_issues.push(CapabilityConfigIssue {
                        capability: "local_fast",
                        reason: String::from("[llama_cpp] table is not configured"),
                    });
                    return Ok(RuntimeConfig {
                        wake_word_model_path,
                        parakeet_model_dir,
                        silero_vad_model,
                        silence_timeout_ms,
                        wake_word_detection_threshold,
                        local_tts,
                        logging,
                        telemetry,
                        response_backend: ResponseBackendConfig::Unconfigured,
                        opencode: opencode.clone(),
                        llama_cpp: llama_cpp.clone(),
                        custom_openai: custom_openai.clone(),
                        completion: completion.clone(),
                        capability_issues,
                    });
                };
                if let Some(config) = llama_cpp.as_ref().filter(|config| {
                    config.server_path.is_file() && config.fast_model_path.is_file()
                }) {
                    ResponseBackendConfig::LlamaCpp {
                        server_path: config.server_path.clone(),
                        host: config.host.clone(),
                        port: config.port,
                        fast_model_path: config.fast_model_path.clone(),
                        quality_model_path: config.quality_model_path.clone(),
                    }
                } else {
                    ResponseBackendConfig::Unconfigured
                }
            }
            None => {
                capability_issues.push(CapabilityConfigIssue {
                    capability: "response_provider",
                    reason: String::from("response_backend is not configured"),
                });
                ResponseBackendConfig::Unconfigured
            }
        };

    Ok(RuntimeConfig {
        wake_word_model_path,
        parakeet_model_dir,
        silero_vad_model,
        silence_timeout_ms,
        wake_word_detection_threshold,
        local_tts,
        logging,
        telemetry,
        response_backend,
        opencode,
        llama_cpp,
        custom_openai,
        completion,
        capability_issues,
    })
}

fn default_runtime_config(config_dir: &Path, config_reason: Option<String>) -> RuntimeConfig {
    let mut capability_issues = Vec::new();
    if let Some(reason) = config_reason {
        capability_issues.push(CapabilityConfigIssue {
            capability: "config",
            reason,
        });
    }
    for capability in [
        "response_provider",
        "wake_word",
        "vad",
        "parakeet",
        "tts",
        "qwen_prediction",
        "deep",
        "review",
    ] {
        capability_issues.push(CapabilityConfigIssue {
            capability,
            reason: String::from("not configured"),
        });
    }
    RuntimeConfig {
        wake_word_model_path: resolve_config_path(
            config_dir,
            PathBuf::from("models/hey_livekit.onnx"),
        ),
        parakeet_model_dir: resolve_config_path(config_dir, PathBuf::from("models/parakeet-v2")),
        silero_vad_model: resolve_config_path(config_dir, PathBuf::from(DEFAULT_SILERO_VAD_MODEL)),
        silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
        wake_word_detection_threshold: DEFAULT_WAKE_WORD_DETECTION_THRESHOLD,
        local_tts: LocalTtsConfig {
            enabled: false,
            model_path: resolve_config_path(config_dir, PathBuf::from("models/tts/jarvis.onnx")),
            worker_count: DEFAULT_TTS_WORKER_COUNT,
            max_queue: DEFAULT_TTS_MAX_QUEUE,
            sample_rate_hz: DEFAULT_TTS_SAMPLE_RATE_HZ,
            max_duration_s: DEFAULT_TTS_MAX_DURATION_S,
            output_gain_db: DEFAULT_TTS_OUTPUT_GAIN_DB,
        },
        logging: LoggingConfig { enabled: false },
        telemetry: TelemetryConfig {
            enabled: true,
            max_bytes: DEFAULT_TELEMETRY_MAX_BYTES,
            backup_count: DEFAULT_TELEMETRY_BACKUP_COUNT,
        },
        response_backend: ResponseBackendConfig::Unconfigured,
        opencode: None,
        llama_cpp: None,
        custom_openai: None,
        completion: None,
        capability_issues,
    }
}

fn record_path_issue(
    issues: &mut Vec<CapabilityConfigIssue>,
    capability: &'static str,
    configured: bool,
    available: bool,
    path: &Path,
) {
    if available {
        return;
    }
    issues.push(CapabilityConfigIssue {
        capability,
        reason: if configured {
            format!("configured asset is unavailable: {}", path.display())
        } else {
            String::from("not configured")
        },
    });
}

fn record_custom_auth_issue(
    issues: &mut Vec<CapabilityConfigIssue>,
    source: CustomOpenAiAuthSource,
    auth_path: &Path,
    windows: bool,
) {
    match source {
        CustomOpenAiAuthSource::Wsl if !windows => issues.push(CapabilityConfigIssue {
            capability: "custom_provider",
            reason: String::from("WSL auth is only supported by the Windows application"),
        }),
        CustomOpenAiAuthSource::Wsl => {}
        CustomOpenAiAuthSource::Native if auth_path.as_os_str().is_empty() => {
            issues.push(CapabilityConfigIssue {
                capability: "custom_provider",
                reason: String::from(
                    "auth path unavailable: HOME or absolute XDG_DATA_HOME is required",
                ),
            });
        }
        CustomOpenAiAuthSource::Native => record_path_issue(
            issues,
            "custom_provider",
            true,
            auth_path.is_file(),
            auth_path,
        ),
    }
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

fn default_telemetry_enabled() -> bool {
    true
}

fn default_telemetry_max_bytes() -> usize {
    DEFAULT_TELEMETRY_MAX_BYTES
}

fn default_telemetry_backup_count() -> u8 {
    DEFAULT_TELEMETRY_BACKUP_COUNT
}

fn default_private_endpoint() -> String {
    String::from("https://chatgpt.com/backend-api/codex/responses")
}

fn valid_custom_endpoint(endpoint: &str) -> bool {
    if endpoint.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let suffix = &rest[authority_end..];
    if authority.contains('@') {
        return false;
    }
    if scheme.eq_ignore_ascii_case("https") {
        return matches!(
            authority.to_ascii_lowercase().as_str(),
            "chatgpt.com" | "chatgpt.com:443"
        ) && suffix == "/backend-api/codex/responses";
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let host = authority
        .strip_prefix('[')
        .and_then(|v| v.split_once(']').map(|x| x.0))
        .unwrap_or_else(|| authority.split(':').next().unwrap_or(""));
    let port_ok = if let Some(ipv6) = authority.strip_prefix('[') {
        ipv6.split_once(']').is_some_and(|(_, rest)| {
            rest.is_empty()
                || rest
                    .strip_prefix(':')
                    .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port != 0))
        })
    } else {
        authority
            .split_once(':')
            .is_none_or(|(_, port)| port.parse::<u16>().is_ok_and(|port| port != 0))
    };
    port_ok
        && host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn default_auth_path() -> PathBuf {
    default_auth_path_from_env(|name| std::env::var_os(name), cfg!(windows))
}

fn default_auth_path_from_env<F>(mut get: F, windows: bool) -> PathBuf
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    if windows {
        get("APPDATA")
            .or_else(|| {
                get("USERPROFILE")
                    .map(|p| PathBuf::from(p).join("AppData/Roaming").into_os_string())
            })
            .map(|base| PathBuf::from(base).join("opencode/auth.json"))
            .unwrap_or_default()
    } else {
        get("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| get("HOME").map(|p| PathBuf::from(p).join(".local/share")))
            .map(|p| p.join("opencode/auth.json"))
            .unwrap_or_default()
    }
}

fn resolve_config_path(config_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn is_absolute_linux_path(path: &Path) -> bool {
    path.to_str().is_some_and(|path| path.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::{
        default_auth_path_from_env, default_private_endpoint, linux_app_dir_from_env,
        load_runtime_config, platform_dirs_from_env, record_custom_auth_issue,
        soul_path_for_config, valid_custom_endpoint, ConfigError, CustomOpenAiAuthSource,
        InferencePolicy, OpencodeRuntime, RawCompletionConfig, RawLlamaCppConfig,
        ResponseBackendConfig,
    };
    use std::collections::HashMap;
    use std::fs;

    #[test]
    fn private_endpoint_defaults_to_chatgpt_codex_responses() {
        assert_eq!(
            default_private_endpoint(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn custom_endpoint_policy_matches_provider_transport_policy() {
        for endpoint in [
            "https://chatgpt.com/backend-api/codex/responses",
            "HTTPS://CHATGPT.COM:443/backend-api/codex/responses",
            "http://127.0.0.1:8080/responses",
            "http://127.0.0.1:8080/responses?tag=a@b",
            "http://[::1]:8080/responses",
        ] {
            assert!(
                valid_custom_endpoint(endpoint),
                "expected valid: {endpoint}"
            );
        }
        for endpoint in [
            "https://chatgpt.com/other",
            "https://chatgpt.com/backend-api/codex/responses?redirect=1",
            "http://example.com/responses",
            "http://localhost:8080/responses",
            "http://user@127.0.0.1/responses",
            "file:///tmp/responses",
        ] {
            assert!(
                !valid_custom_endpoint(endpoint),
                "expected invalid: {endpoint}"
            );
        }
    }

    #[test]
    fn custom_endpoint_is_normalized_and_invalid_policy_is_reported() {
        let temp = TempDir::new();
        let config_path = temp.path().join("config.toml");
        let auth_path = temp.path().join("auth.json");
        create_file(&auth_path);
        fs::write(
            &config_path,
            format!(
                "[custom_openai]\nendpoint = \"  https://example.test/responses  \"\nauth_path = \"{}\"\n",
                escape_path(&auth_path),
            ),
        )
        .expect("config fixture should be written");

        let config = load_runtime_config(Some(&config_path))
            .expect("invalid optional endpoint should not block other capabilities");

        assert_eq!(
            config
                .custom_openai
                .as_ref()
                .expect("custom config")
                .endpoint,
            "https://example.test/responses",
        );
        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "custom_provider"
                && issue.reason == "custom_openai.endpoint is invalid"
        }));
    }

    #[test]
    fn provider_modes_default_to_native_and_preserve_native_paths() {
        let temp = TempDir::new();
        let auth = temp.path().join("auth.json");
        let opencode = temp.path().join("opencode");
        create_file(&auth);
        create_file(&opencode);
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            format!(
                "response_backend = \"opencode\"\n[opencode]\npath = \"{}\"\n[custom_openai]\nauth_path = \"{}\"\n",
                escape_path(&opencode),
                escape_path(&auth)
            ),
        )
        .expect("config should be written");

        let config = load_runtime_config(Some(&path)).expect("native config should load");
        assert_eq!(config.opencode.unwrap().runtime, OpencodeRuntime::Native);
        assert_eq!(
            config.custom_openai.unwrap().auth_source,
            CustomOpenAiAuthSource::Native
        );
    }

    #[test]
    fn wsl_modes_keep_omitted_paths_unresolved() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "response_backend = \"opencode\"\n[opencode]\nruntime = \"wsl\"\n[custom_openai]\nauth_source = \"wsl\"\n",
        )
        .expect("config should be written");

        let config = load_runtime_config(Some(&path)).expect("WSL config should parse");
        assert_eq!(config.opencode.unwrap().path, PathBuf::new());
        assert_eq!(config.custom_openai.unwrap().auth_path, PathBuf::new());
        #[cfg(not(target_os = "windows"))]
        {
            assert!(config.capability_issues.iter().any(|issue| {
                issue.capability == "opencode" && issue.reason.contains("only supported")
            }));
            assert!(config.capability_issues.iter().any(|issue| {
                issue.capability == "custom_provider" && issue.reason.contains("only supported")
            }));
        }
        assert!(matches!(
            config.response_backend,
            ResponseBackendConfig::Opencode { path } if path.as_os_str().is_empty()
        ));
    }

    #[test]
    fn rejects_relative_wsl_provider_paths() {
        for config in [
            "[opencode]\nruntime = \"wsl\"\npath = \"relative/opencode\"\n",
            "[custom_openai]\nauth_source = \"wsl\"\nauth_path = \"relative/auth.json\"\n",
        ] {
            let temp = TempDir::new();
            let path = temp.path().join("config.toml");
            fs::write(&path, config).expect("config should be written");
            assert!(matches!(
                load_runtime_config(Some(&path)),
                Err(ConfigError::ParseConfigFailed { details, .. })
                    if details.contains("absolute Linux path")
            ));
        }
    }

    #[test]
    fn preserves_explicit_wsl_provider_paths_verbatim() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "[opencode]\nruntime = \"wsl\"\npath = \"/home/user/bin/opencode\"\n[custom_openai]\nauth_source = \"wsl\"\nauth_path = \"/home/user/auth.json\"\n",
        )
        .expect("config should be written");

        let config = load_runtime_config(Some(&path)).expect("WSL config should parse");
        assert_eq!(
            config.opencode.expect("OpenCode config").path,
            PathBuf::from("/home/user/bin/opencode")
        );
        assert_eq!(
            config.custom_openai.expect("Custom config").auth_path,
            PathBuf::from("/home/user/auth.json")
        );
    }

    #[test]
    fn windows_defers_wsl_auth_file_validation_to_the_platform_resolver() {
        for path in [Path::new(""), Path::new("/home/user/missing-auth.json")] {
            let mut issues = Vec::new();
            record_custom_auth_issue(&mut issues, CustomOpenAiAuthSource::Wsl, path, true);
            assert!(issues.is_empty());
        }
    }

    #[test]
    fn rejects_native_opencode_without_a_path() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(&path, "[opencode]\n").expect("config should be written");

        assert!(matches!(
            load_runtime_config(Some(&path)),
            Err(ConfigError::ParseConfigFailed { details, .. })
                if details.contains("opencode.path is required")
        ));
    }
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resolves_linux_xdg_directories_without_process_environment() {
        let vars = HashMap::from([
            ("HOME", "/home/test"),
            ("XDG_CONFIG_HOME", "/config"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CACHE_HOME", "/cache"),
        ]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), false)
                .expect("complete Linux environment should resolve");
        assert_eq!(dirs.config, PathBuf::from("/config/voxgolem"));
        assert_eq!(dirs.data, PathBuf::from("/data/voxgolem"));
        assert_eq!(dirs.state, PathBuf::from("/state/voxgolem"));
        assert_eq!(dirs.cache, PathBuf::from("/cache/voxgolem"));
    }

    #[test]
    fn resolves_linux_defaults_from_home() {
        let vars = HashMap::from([("HOME", "/home/test")]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), false)
                .expect("HOME should provide Linux defaults");
        assert_eq!(dirs.config, PathBuf::from("/home/test/.config/voxgolem"));
        assert_eq!(dirs.data, PathBuf::from("/home/test/.local/share/voxgolem"));
        assert_eq!(
            dirs.state,
            PathBuf::from("/home/test/.local/state/voxgolem")
        );
        assert_eq!(dirs.cache, PathBuf::from("/home/test/.cache/voxgolem"));
    }

    #[test]
    fn resolves_complete_linux_xdg_directories_without_home() {
        let vars = HashMap::from([
            ("XDG_CONFIG_HOME", "/config"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_CACHE_HOME", "/cache"),
        ]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), false)
                .expect("absolute XDG directories should not require HOME");

        assert_eq!(dirs.config, PathBuf::from("/config/voxgolem"));
        assert_eq!(dirs.data, PathBuf::from("/data/voxgolem"));
        assert_eq!(dirs.state, PathBuf::from("/state/voxgolem"));
        assert_eq!(dirs.cache, PathBuf::from("/cache/voxgolem"));
    }

    #[test]
    fn resolves_one_linux_xdg_directory_without_unrelated_environment() {
        let vars = HashMap::from([("XDG_CONFIG_HOME", "/config")]);
        assert_eq!(
            linux_app_dir_from_env(
                &mut |name| vars.get(name).map(std::ffi::OsString::from),
                "XDG_CONFIG_HOME",
                ".config",
            )
            .unwrap(),
            PathBuf::from("/config/voxgolem")
        );
    }

    #[test]
    fn ignores_relative_linux_xdg_directories() {
        let vars = HashMap::from([
            ("HOME", "/home/test"),
            ("XDG_CONFIG_HOME", "relative-config"),
            ("XDG_DATA_HOME", "relative-data"),
            ("XDG_STATE_HOME", "relative-state"),
            ("XDG_CACHE_HOME", "relative-cache"),
        ]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), false)
                .expect("HOME should provide fallbacks for relative XDG values");

        assert_eq!(dirs.config, PathBuf::from("/home/test/.config/voxgolem"));
        assert_eq!(dirs.data, PathBuf::from("/home/test/.local/share/voxgolem"));
        assert_eq!(
            dirs.state,
            PathBuf::from("/home/test/.local/state/voxgolem")
        );
        assert_eq!(dirs.cache, PathBuf::from("/home/test/.cache/voxgolem"));
    }

    #[test]
    fn resolves_soul_beside_overridden_config() {
        assert_eq!(
            soul_path_for_config(PathBuf::from("/opt/vox/config.toml")),
            PathBuf::from("/opt/vox/SOUL.md")
        );
    }

    #[test]
    fn resolves_windows_appdata_and_localappdata() {
        let vars = HashMap::from([
            ("APPDATA", r"C:\Users\test\AppData\Roaming"),
            ("LOCALAPPDATA", r"C:\Users\test\AppData\Local"),
        ]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), true)
                .expect("Windows environment should resolve");
        assert_eq!(
            dirs.config,
            PathBuf::from(r"C:\Users\test\AppData\Roaming").join("VoxGolem")
        );
        assert_eq!(
            dirs.data,
            PathBuf::from(r"C:\Users\test\AppData\Local").join("VoxGolem")
        );
        assert_eq!(dirs.state, dirs.data);
        assert_eq!(dirs.cache, dirs.data);
    }

    #[test]
    fn resolves_windows_directories_from_userprofile() {
        let vars = HashMap::from([("USERPROFILE", r"C:\Users\test")]);
        let dirs =
            platform_dirs_from_env(|name| vars.get(name).map(std::ffi::OsString::from), true)
                .expect("USERPROFILE should provide Windows directory fallbacks");

        assert_eq!(
            dirs.config,
            PathBuf::from(r"C:\Users\test")
                .join("AppData/Roaming")
                .join("VoxGolem")
        );
        assert_eq!(
            dirs.data,
            PathBuf::from(r"C:\Users\test")
                .join("AppData/Local")
                .join("VoxGolem")
        );
    }

    #[test]
    fn resolves_default_auth_paths_from_platform_data_directories() {
        let linux = HashMap::from([("XDG_DATA_HOME", "/data")]);
        assert_eq!(
            default_auth_path_from_env(|name| linux.get(name).map(std::ffi::OsString::from), false),
            PathBuf::from("/data/opencode/auth.json")
        );
        let windows = HashMap::from([("USERPROFILE", r"C:\Users\test")]);
        assert_eq!(
            default_auth_path_from_env(
                |name| windows.get(name).map(std::ffi::OsString::from),
                true
            ),
            PathBuf::from(r"C:\Users\test")
                .join("AppData/Roaming")
                .join("opencode/auth.json")
        );
        assert_eq!(default_auth_path_from_env(|_| None, false), PathBuf::new());
        assert_eq!(default_auth_path_from_env(|_| None, true), PathBuf::new());
    }

    #[test]
    fn loads_repository_config_example_through_runtime_parser() {
        let config_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("config.example.toml");

        let config = load_runtime_config(Some(&config_path))
            .expect("repository config.example.toml should deserialize");

        assert_eq!(config.silence_timeout_ms, 1_500);
        assert_eq!(config.wake_word_detection_threshold, 0.68);
        assert!(config.telemetry.enabled);
        let llama = config
            .llama_cpp
            .expect("example config should include llama_cpp settings");
        assert_eq!(llama.host, "127.0.0.1");
        assert_eq!(llama.port, 11_435);
    }

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
    fn reports_missing_config_as_a_capability_issue() {
        let temp = TempDir::new();
        let missing_path = temp.path().join("missing.toml");

        let result = load_runtime_config(Some(&missing_path));

        let config = result.expect("missing config must produce a zero-asset runtime config");
        assert_eq!(config.response_backend, ResponseBackendConfig::Unconfigured);
        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "config"
                && issue.reason.contains(&missing_path.display().to_string())
        }));
    }

    #[test]
    fn defaults_telemetry_when_config_is_absent() {
        let temp = TempDir::new();
        let config = load_runtime_config(Some(&temp.path().join("missing.toml")))
            .expect("missing config should use defaults");

        assert_eq!(
            config.telemetry,
            super::TelemetryConfig {
                enabled: true,
                max_bytes: 10 * 1024 * 1024,
                backup_count: 3,
            }
        );
        assert!(!config.logging.enabled);
    }

    #[test]
    fn validates_standalone_llama_loopback_host_without_selected_backend() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "response_backend = \"opencode\"\n[llama_cpp]\nserver_path = \"missing\"\nhost = \"0.0.0.0\"\nport = 11435\nfast_model_path = \"fast\"\n",
        )
        .expect("config should be written");

        assert!(matches!(
            load_runtime_config(Some(&path)),
            Err(ConfigError::ParseConfigFailed { details, .. })
                if details.contains("llama_cpp.host must be a supported loopback host")
        ));
    }

    #[test]
    fn validates_standalone_llama_port_without_selected_backend() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "[llama_cpp]\nserver_path = \"missing\"\nhost = \"127.0.0.1\"\nport = 0\nfast_model_path = \"fast\"\n",
        )
        .expect("config should be written");

        assert!(matches!(
            load_runtime_config(Some(&path)),
            Err(ConfigError::ParseConfigFailed { details, .. })
                if details.contains("llama_cpp.port must be greater than zero")
        ));
    }

    #[test]
    fn loads_explicit_telemetry_values() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "[telemetry]\nenabled = false\nmax_bytes = 2048\nbackup_count = 0\n",
        )
        .expect("telemetry config should be written");

        let config = load_runtime_config(Some(&path)).expect("valid telemetry config should load");
        assert!(!config.telemetry.enabled);
        assert_eq!(config.telemetry.max_bytes, 2048);
        assert_eq!(config.telemetry.backup_count, 0);
    }

    #[test]
    fn rejects_unknown_telemetry_fields() {
        let temp = TempDir::new();
        let path = temp.path().join("config.toml");
        fs::write(&path, "[telemetry]\nunknown = true\n").expect("config should be written");

        assert!(matches!(
            load_runtime_config(Some(&path)),
            Err(ConfigError::ParseConfigFailed { .. })
        ));
    }

    #[test]
    fn rejects_invalid_telemetry_bounds() {
        for (field, value) in [("max_bytes", "1023"), ("backup_count", "11")] {
            let temp = TempDir::new();
            let path = temp.path().join("config.toml");
            fs::write(&path, format!("[telemetry]\n{field} = {value}\n"))
                .expect("config should be written");

            assert!(matches!(
                load_runtime_config(Some(&path)),
                Err(ConfigError::ParseConfigFailed { details, .. })
                    if details.starts_with("telemetry.")
            ));
        }
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
    fn reports_unavailable_configured_wake_word_model_file() {
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

        let config = result.expect("missing wake word asset must not block other capabilities");
        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "wake_word"
                && issue
                    .reason
                    .contains(&missing_wake_word_model_path.display().to_string())
        }));
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
        assert!(!result.logging.enabled);
    }

    #[test]
    fn defaults_runtime_file_logging_to_disabled() {
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

        assert!(!result.logging.enabled);
    }

    #[test]
    fn loads_runtime_file_logging_when_enabled() {
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
                "{}\n[logging]\nenabled = true\n",
                render_opencode_config(
                    &wake_word_model_path,
                    &model_dir,
                    &silero_vad_model,
                    &opencode_path,
                )
            ),
        )
        .expect("config fixture should be written");

        let result = load_runtime_config(Some(&config_path)).expect("valid config should load");

        assert!(result.logging.enabled);
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
    fn reports_unavailable_configured_silero_vad_model_file() {
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

        let config = result.expect("missing VAD asset must not block other capabilities");
        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "vad"
                && issue
                    .reason
                    .contains(&missing_silero_vad_model.display().to_string())
        }));
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

        let config = result.expect("missing optional llama table must remain nonfatal");
        assert_eq!(config.response_backend, ResponseBackendConfig::Unconfigured);
        assert!(config
            .capability_issues
            .iter()
            .any(|issue| issue.capability == "local_fast"));
    }

    #[test]
    fn selected_llama_backend_with_missing_assets_is_unconfigured_once() {
        let temp = TempDir::new();
        let config_path = temp.path().join("config.toml");
        fs::write(
            &config_path,
            "response_backend = \"llama_cpp\"\n[llama_cpp]\nserver_path = \"missing-server\"\nhost = \"127.0.0.1\"\nport = 11435\nfast_model_path = \"missing-model\"\n",
        )
        .expect("config fixture should be written");

        let config = load_runtime_config(Some(&config_path)).expect("config should load");

        assert_eq!(config.response_backend, ResponseBackendConfig::Unconfigured);
        assert_eq!(
            config
                .capability_issues
                .iter()
                .filter(|issue| issue.capability == "local_fast")
                .count(),
            1
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

        let config = result.expect("missing optional OpenCode table must remain nonfatal");
        assert_eq!(config.response_backend, ResponseBackendConfig::Unconfigured);
        assert!(config
            .capability_issues
            .iter()
            .any(|issue| issue.capability == "opencode"));
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
    fn reports_unavailable_configured_tts_model_file() {
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

        let config = result.expect("missing TTS asset must not block response capabilities");
        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "tts"
                && issue
                    .reason
                    .contains(&missing_tts_model_path.display().to_string())
        }));
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

    #[test]
    fn reports_quality_unavailable_when_shared_llama_server_is_missing() {
        let temp = TempDir::new();
        let missing_server = temp.path().join("missing-llama-server");
        let fast_model = temp.path().join("fast.gguf");
        let quality_model = temp.path().join("quality.gguf");
        let config_path = temp.path().join("config.toml");
        create_file(&fast_model);
        create_file(&quality_model);
        fs::write(
            &config_path,
            format!(
                "[llama_cpp]\nserver_path = \"{}\"\nhost = \"127.0.0.1\"\nport = 11435\nfast_model_path = \"{}\"\nquality_model_path = \"{}\"\n",
                escape_path(&missing_server),
                escape_path(&fast_model),
                escape_path(&quality_model),
            ),
        )
        .expect("config fixture should be written");

        let config = load_runtime_config(Some(&config_path)).expect("config should load");

        assert!(config.capability_issues.iter().any(|issue| {
            issue.capability == "local_quality"
                && issue.reason.contains(&missing_server.display().to_string())
        }));
    }

    #[test]
    fn inference_provider_defaults_to_auto_independently() {
        let llama: RawLlamaCppConfig = toml::from_str(
            "server_path = 'server'\nhost = 'localhost'\nport = 1\nfast_model_path = 'fast'",
        )
        .expect("llama_cpp config should parse");
        let completion: RawCompletionConfig =
            toml::from_str("server_path = 'server'\nmodel_path = 'model'")
                .expect("completion config should parse");

        assert_eq!(llama.inference_provider, InferencePolicy::Auto);
        assert_eq!(completion.inference_provider, InferencePolicy::Auto);
    }

    #[test]
    fn inference_provider_accepts_explicit_values() {
        for (value, expected) in [
            ("auto", InferencePolicy::Auto),
            ("cuda", InferencePolicy::Cuda),
            ("cpu", InferencePolicy::Cpu),
        ] {
            let config: RawCompletionConfig = toml::from_str(&format!(
                "server_path = 'server'\nmodel_path = 'model'\ninference_provider = '{value}'"
            ))
            .expect("inference provider should parse");
            assert_eq!(config.inference_provider, expected);
        }
    }

    #[test]
    fn inference_provider_rejects_invalid_values() {
        let result: Result<RawCompletionConfig, _> = toml::from_str(
            "server_path = 'server'\nmodel_path = 'model'\ninference_provider = 'metal'",
        );
        assert!(result.is_err());
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
