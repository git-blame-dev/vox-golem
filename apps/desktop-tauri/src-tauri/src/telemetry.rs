//! Metadata-only, local JSONL telemetry.

use serde::Serialize;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
const O_DIRECTORY: i32 = 0o200000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;

const FILE_NAME: &str = "telemetry.jsonl";
pub const SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Voice,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Local,
    Custom,
    OpenCode,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Generation,
    PartialTranscription,
    CompletionPrediction,
    Prefetch,
    Deep,
    Review,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    InProcess,
    Http,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceProvider {
    Cuda,
    Cpu,
    AttachedUnknown,
    Remote,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculativeOrigin {
    None,
    User,
    System,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Error,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    None,
    Internal,
}

#[derive(Clone, Debug, Serialize)]
pub struct TelemetryMetadata {
    pub schema_version: u8,
    pub timestamp_ms: u64,
    pub request_id: String,
    pub generation: u64,
    pub input_source: InputSource,
    pub provider: Provider,
    pub model: String,
    pub stage: Stage,
    pub transport: Transport,
    pub inference_provider: InferenceProvider,
    pub speculative_origin: SpeculativeOrigin,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub status: Status,
    pub error_category: ErrorCategory,
}

#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub max_bytes: u64,
    pub backup_count: usize,
}

pub struct TelemetrySink {
    dir: PathBuf,
    config: TelemetryConfig,
}

impl TelemetrySink {
    pub fn new(state_dir: impl Into<PathBuf>, config: TelemetryConfig) -> Self {
        Self {
            dir: state_dir.into(),
            config,
        }
    }

    pub fn append(&self, metadata: &TelemetryMetadata) -> io::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        validate_identifier(&metadata.request_id)?;
        validate_identifier(&metadata.model)?;
        let line = serde_json::to_vec(metadata).map_err(io::Error::other)?;
        let line_len = line.len() as u64 + 1;
        if line_len > self.config.max_bytes {
            return Err(io::Error::other("telemetry record exceeds rotation limit"));
        }
        let (_directory_handle, directory_path) = prepare_directory(&self.dir)?;
        let path = directory_path.join(FILE_NAME);
        reject_symlink(&path)?;
        let current_len = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if current_len.saturating_add(line_len) > self.config.max_bytes {
            self.rotate(&directory_path)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        #[cfg(target_os = "linux")]
        options.custom_flags(O_NOFOLLOW);
        let mut file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry path must be a regular file",
            ));
        }
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&line)?;
        file.write_all(b"\n")
    }

    fn rotate(&self, directory_path: &std::path::Path) -> io::Result<()> {
        let path = directory_path.join(FILE_NAME);
        if self.config.backup_count == 0 {
            return remove_if_present(path);
        }
        let oldest = directory_path.join(format!("{FILE_NAME}.{}", self.config.backup_count));
        remove_if_present(oldest)?;
        for index in (1..self.config.backup_count).rev() {
            let from = directory_path.join(format!("{FILE_NAME}.{index}"));
            let to = directory_path.join(format!("{FILE_NAME}.{}", index + 1));
            if path_exists(&from)? {
                fs::rename(from, to)?;
            }
        }
        if path_exists(&path)? {
            fs::rename(path, directory_path.join(format!("{FILE_NAME}.1")))?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn prepare_directory(path: &std::path::Path) -> io::Result<(File, PathBuf)> {
    reject_symlink(path)?;
    fs::create_dir_all(path)?;
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(O_DIRECTORY | O_NOFOLLOW);
    let directory = options.open(path)?;
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "telemetry directory must be a directory",
        ));
    }
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    let anchored_path = PathBuf::from(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(&directory)
    ));
    Ok((directory, anchored_path))
}

#[cfg(not(target_os = "linux"))]
fn prepare_directory(path: &std::path::Path) -> io::Result<((), PathBuf)> {
    reject_symlink(path)?;
    fs::create_dir_all(path)?;
    reject_symlink(path)?;
    set_directory_permissions(path)?;
    Ok(((), path.to_path_buf()))
}

fn path_exists(path: &std::path::Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "telemetry path must not be a symlink",
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn reject_symlink(path: &std::path::Path) -> io::Result<()> {
    path_exists(path).map(|_| ())
}

fn remove_if_present(path: PathBuf) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn set_directory_permissions(path: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_identifier(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "telemetry identifier is invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::tempdir;

    fn event() -> TelemetryMetadata {
        TelemetryMetadata {
            schema_version: SCHEMA_VERSION,
            timestamp_ms: 1,
            request_id: "req-1".into(),
            generation: 1,
            input_source: InputSource::Voice,
            provider: Provider::Local,
            model: "model".into(),
            stage: Stage::CompletionPrediction,
            transport: Transport::InProcess,
            inference_provider: InferenceProvider::Cpu,
            speculative_origin: SpeculativeOrigin::System,
            input_tokens: Some(2),
            output_tokens: Some(3),
            duration_ms: Some(4),
            status: Status::Ok,
            error_category: ErrorCategory::None,
        }
    }
    fn config(max_bytes: u64, backup_count: usize) -> TelemetryConfig {
        TelemetryConfig {
            enabled: true,
            max_bytes,
            backup_count,
        }
    }

    #[test]
    fn appends_jsonl() {
        let dir = tempdir().unwrap();
        TelemetrySink::new(dir.path(), config(4096, 2))
            .append(&event())
            .unwrap();
        let text = fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 1);
        assert!(serde_json::from_str::<serde_json::Value>(text.trim_end()).is_ok());
    }
    #[test]
    fn disabled_does_not_write() {
        let dir = tempdir().unwrap();
        TelemetrySink::new(
            dir.path(),
            TelemetryConfig {
                enabled: false,
                ..config(20, 1)
            },
        )
        .append(&event())
        .unwrap();
        assert!(!dir.path().join(FILE_NAME).exists());
    }
    #[test]
    fn rotates_with_fixed_backups() {
        let dir = tempdir().unwrap();
        let record_len = serde_json::to_vec(&event()).unwrap().len() as u64 + 1;
        let sink = TelemetrySink::new(dir.path(), config(record_len + 1, 1));
        sink.append(&event()).unwrap();
        sink.append(&event()).unwrap();
        assert!(dir.path().join("telemetry.jsonl.1").exists());
    }
    #[test]
    fn zero_backups_discards_previous_file_without_retaining_backup() {
        let dir = tempdir().unwrap();
        let record_len = serde_json::to_vec(&event()).unwrap().len() as u64 + 1;
        let sink = TelemetrySink::new(dir.path(), config(record_len + 1, 0));
        sink.append(&event()).unwrap();
        sink.append(&event()).unwrap();
        assert!(dir.path().join(FILE_NAME).exists());
        assert!(!dir.path().join("telemetry.jsonl.1").exists());
    }

    #[test]
    fn serializes_exact_metadata_schema() {
        let value = serde_json::to_value(event()).unwrap();
        let keys: std::collections::BTreeSet<_> =
            value.as_object().unwrap().keys().cloned().collect();
        let expected = [
            "schema_version",
            "timestamp_ms",
            "request_id",
            "generation",
            "input_source",
            "provider",
            "model",
            "stage",
            "transport",
            "inference_provider",
            "speculative_origin",
            "input_tokens",
            "output_tokens",
            "duration_ms",
            "status",
            "error_category",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(keys, expected);
    }

    #[cfg(unix)]
    #[test]
    fn uses_owner_only_directory_and_file_permissions() {
        let dir = tempdir().unwrap();
        TelemetrySink::new(dir.path(), config(4096, 1))
            .append(&event())
            .unwrap();
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.path().join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_symlinked_directory_and_file_paths() {
        let root = tempdir().unwrap();
        let real_directory = root.path().join("real");
        fs::create_dir(&real_directory).unwrap();
        let linked_directory = root.path().join("linked");
        symlink(&real_directory, &linked_directory).unwrap();
        assert!(TelemetrySink::new(&linked_directory, config(4096, 1))
            .append(&event())
            .is_err());

        let target = root.path().join("target");
        fs::write(&target, "unchanged").unwrap();
        symlink(&target, real_directory.join(FILE_NAME)).unwrap();
        assert!(TelemetrySink::new(&real_directory, config(4096, 1))
            .append(&event())
            .is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "unchanged");
    }
    #[test]
    fn sentinel_is_not_serialized() {
        let dir = tempdir().unwrap();
        let mut value = event();
        value.request_id = "safe".into();
        sink_append(dir.path(), &value);
        let text = fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(!text.contains("PROMPT_SENTINEL"));
    }
    fn sink_append(dir: &Path, event: &TelemetryMetadata) {
        TelemetrySink::new(dir, config(4096, 1))
            .append(event)
            .unwrap();
    }
}
