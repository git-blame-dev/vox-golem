#[cfg(test)]
mod tests {
    use super::*;
    const REVISION: &str = "0123456789abcdef";

    #[test]
    fn detected_clip_is_grouped_by_model_and_can_be_reviewed() {
        let root = tempfile::tempdir().unwrap();
        let clip = save_clip(
            root.path(),
            "hey_livekit.onnx",
            REVISION,
            &[0.0, 0.5, -0.5],
            0.82,
        )
        .unwrap();
        assert_eq!(clip.profile, "hey_livekit-0123456789abcdef");
        assert_eq!(clip.model_revision, REVISION);
        assert_eq!(clip.label, ClipLabel::Unreviewed);
        assert!(root
            .path()
            .join(&clip.profile)
            .join(format!("{}.wav", clip.id))
            .is_file());
        assert_eq!(list_clips(root.path(), 0).unwrap().len(), 1);
        let changed = set_label(root.path(), &clip.profile, &clip.id, ClipLabel::TrueWake).unwrap();
        assert_eq!(changed.label, ClipLabel::TrueWake);
        assert!(read_clip(root.path(), &clip.profile, &clip.id)
            .unwrap()
            .starts_with(b"RIFF"));
        let mut wav = hound::WavReader::open(
            root.path()
                .join(&clip.profile)
                .join(format!("{}.wav", clip.id)),
        )
        .unwrap();
        assert_eq!(wav.spec().sample_rate, 16_000);
        assert_eq!(
            wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap(),
            [0, 16_383, -16_383]
        );
        delete_clip(root.path(), &clip.profile, &clip.id).unwrap();
        assert!(list_clips(root.path(), 0).unwrap().is_empty());
    }

    #[test]
    fn rejects_invalid_audio_and_file_identifiers() {
        let root = tempfile::tempdir().unwrap();
        assert!(save_clip(root.path(), "model.onnx", REVISION, &[f32::NAN], 0.9).is_err());
        assert!(save_clip(root.path(), "model.onnx", REVISION, &[], 0.9).is_err());
        assert!(read_clip(root.path(), "../unsafe", "bad").is_err());
        assert!(validate_directory(Path::new("relative/clips")).is_err());
        assert!(validate_directory(&root.path().join("..").join("clips")).is_err());
    }

    #[test]
    fn configured_directory_and_toggle_survive_reload_without_deleting_clips() {
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("wake-clips.json");
        let first = temp.path().join("recordings-one");
        let second = temp.path().join("recordings-two");
        let first_settings =
            set_settings_at(&settings_path, true, first.to_str().unwrap()).unwrap();
        save_clip(&first, "hey_livekit.onnx", REVISION, &[0.1], 0.9).unwrap();
        assert_eq!(
            load_settings_from(&settings_path, &second).unwrap(),
            first_settings
        );
        let disabled = set_settings_at(&settings_path, false, second.to_str().unwrap()).unwrap();
        assert!(!load_settings_from(&settings_path, &first).unwrap().enabled);
        assert_eq!(disabled.directory, second.to_string_lossy());
        assert_eq!(list_clips(&first, 0).unwrap().len(), 1);
    }

    #[test]
    fn oldest_recordings_remain_reviewable_after_a_hundred_detections() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        for _ in 0..100 {
            save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        }
        assert_eq!(list_clips(root.path(), 0).unwrap().len(), 100);
        let older = list_clips(root.path(), 100).unwrap();
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].id, first.id);
        assert_eq!(
            set_label(root.path(), &first.profile, &first.id, ClipLabel::FalseWake)
                .unwrap()
                .label,
            ClipLabel::FalseWake
        );
    }

    #[test]
    fn first_page_does_not_open_metadata_for_the_entire_uncapped_collection() {
        let root = tempfile::tempdir().unwrap();
        for _ in 0..250 {
            save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        }
        let mut reads = 0;
        let page = list_clips_with(root.path(), 0, |root, profile, id| {
            reads += 1;
            read_metadata(root, profile, id)
        })
        .unwrap();
        assert_eq!(page.len(), 100);
        assert_eq!(reads, 100);
    }

    #[test]
    fn missing_index_rebuilds_surviving_recordings_before_new_saves() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        fs::remove_file(root.path().join(INDEX_FILE)).unwrap();
        assert_eq!(list_clips(root.path(), 0).unwrap()[0].id, first.id);
        save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        assert_eq!(list_clips(root.path(), 0).unwrap().len(), 2);
    }

    #[test]
    fn interrupted_index_tail_is_rebuilt_before_next_append() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        OpenOptions::new()
            .append(true)
            .open(root.path().join(INDEX_FILE))
            .unwrap()
            .write_all(b"{\"profile\":\"")
            .unwrap();
        let second = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        let ids = list_clips(root.path(), 0)
            .unwrap()
            .into_iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [second.id, first.id]);
    }

    #[test]
    fn indexed_audio_recovers_metadata_after_interrupted_save() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        let mut interrupted = first.clone();
        interrupted.id.push_str("-recovered");
        interrupted.created_ms += 1;
        append_index(root.path(), &interrupted).unwrap();
        fs::copy(
            clip_path(root.path(), &first.profile, &first.id, "wav").unwrap(),
            clip_path(root.path(), &interrupted.profile, &interrupted.id, "wav").unwrap(),
        )
        .unwrap();
        assert!(
            !clip_path(root.path(), &interrupted.profile, &interrupted.id, "json")
                .unwrap()
                .exists()
        );
        let clips = list_clips(root.path(), 0).unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, interrupted.id);
        assert_eq!(
            set_label(
                root.path(),
                &interrupted.profile,
                &interrupted.id,
                ClipLabel::TrueWake
            )
            .unwrap()
            .label,
            ClipLabel::TrueWake
        );
    }

    #[test]
    fn torn_later_index_append_keeps_an_earlier_indexed_wav_without_sidecar() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        let mut interrupted = first.clone();
        interrupted.id.push_str("-recovered");
        interrupted.created_ms += 1;
        append_index(root.path(), &interrupted).unwrap();
        fs::copy(
            clip_path(root.path(), &first.profile, &first.id, "wav").unwrap(),
            clip_path(root.path(), &interrupted.profile, &interrupted.id, "wav").unwrap(),
        )
        .unwrap();
        OpenOptions::new()
            .append(true)
            .open(root.path().join(INDEX_FILE))
            .unwrap()
            .write_all(b"{\"metadata\":")
            .unwrap();
        let ids = list_clips(root.path(), 0)
            .unwrap()
            .into_iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [interrupted.id.clone(), first.id]);
        assert_eq!(
            read_metadata(root.path(), &interrupted.profile, &interrupted.id)
                .unwrap()
                .id,
            interrupted.id
        );
    }

    #[test]
    fn torn_unicode_index_tail_keeps_an_earlier_indexed_wav_without_sidecar() {
        let root = tempfile::tempdir().unwrap();
        let first = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        let mut interrupted = first.clone();
        interrupted.id.push_str("-recovered");
        interrupted.created_ms += 1;
        append_index(root.path(), &interrupted).unwrap();
        fs::copy(
            clip_path(root.path(), &first.profile, &first.id, "wav").unwrap(),
            clip_path(root.path(), &interrupted.profile, &interrupted.id, "wav").unwrap(),
        )
        .unwrap();
        let mut unicode = first.clone();
        unicode.model_file = String::from("模型.onnx");
        let encoded = serde_json::to_vec(&IndexEntry { metadata: unicode }).unwrap();
        let first_multibyte = encoded.iter().position(|byte| *byte == 0xe6).unwrap();
        OpenOptions::new()
            .append(true)
            .open(root.path().join(INDEX_FILE))
            .unwrap()
            .write_all(&encoded[..first_multibyte + 1])
            .unwrap();
        let ids = list_clips(root.path(), 0)
            .unwrap()
            .into_iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [interrupted.id, first.id]);
    }

    #[test]
    fn long_unicode_model_filename_remains_browsable() {
        let root = tempfile::tempdir().unwrap();
        let filename = format!("{}.onnx", "é".repeat(900));
        let clip = save_clip(root.path(), &filename, REVISION, &[0.1], 0.8).unwrap();
        assert!(
            fs::read_to_string(root.path().join(INDEX_FILE))
                .unwrap()
                .len()
                > 1024
        );
        assert_eq!(list_clips(root.path(), 0).unwrap()[0].id, clip.id);
    }

    #[test]
    fn deleting_many_recent_clips_keeps_page_zero_bounded() {
        let root = tempfile::tempdir().unwrap();
        let clips = (0..250)
            .map(|_| save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap())
            .collect::<Vec<_>>();
        for clip in clips.iter().skip(100) {
            delete_clip(root.path(), &clip.profile, &clip.id).unwrap();
        }
        let mut reads = 0;
        let page = list_clips_with(root.path(), 0, |root, profile, id| {
            reads += 1;
            read_metadata(root, profile, id)
        })
        .unwrap();
        assert_eq!(page.len(), 100);
        assert_eq!(reads, 100);
        assert_eq!(
            fs::read_to_string(root.path().join(INDEX_FILE))
                .unwrap()
                .lines()
                .count(),
            100
        );
    }

    #[test]
    fn disabling_collection_does_not_require_the_configured_directory_to_exist() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("unavailable-drive").join("wake-clips");
        let settings = set_settings_at(
            &temp.path().join("state.json"),
            false,
            missing.to_str().unwrap(),
        )
        .unwrap();
        assert!(!settings.enabled);
        assert!(!missing.exists());
        assert_eq!(
            load_settings_from(&temp.path().join("state.json"), &missing).unwrap(),
            settings
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_model_folder_symlink_when_reading_or_deleting_clips() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let clip = save_clip(other.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        std::os::unix::fs::symlink(
            other.path().join(&clip.profile),
            root.path().join(&clip.profile),
        )
        .unwrap();
        assert!(read_clip(root.path(), &clip.profile, &clip.id).is_err());
        assert!(delete_clip(root.path(), &clip.profile, &clip.id).is_err());
        assert!(other
            .path()
            .join(&clip.profile)
            .join(format!("{}.wav", clip.id))
            .is_file());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_configured_path_through_symlinked_parent_into_repository() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::create_dir(project.join(".git")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let clips = alias.join("new-clips");
        assert!(validate_directory(&clips).is_err());
        assert!(!project.join("new-clips").exists());
    }

    #[test]
    fn model_replacement_under_same_filename_uses_a_separate_group() {
        let root = tempfile::tempdir().unwrap();
        let earlier = save_clip(root.path(), "hey_livekit.onnx", REVISION, &[0.1], 0.8).unwrap();
        let updated = save_clip(
            root.path(),
            "hey_livekit.onnx",
            "fedcba9876543210",
            &[0.1],
            0.8,
        )
        .unwrap();
        assert_ne!(earlier.profile, updated.profile);
        assert_ne!(earlier.model_revision, updated.model_revision);
        assert_eq!(list_clips(root.path(), 0).unwrap().len(), 2);
    }
}
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const SETTINGS_FILE: &str = "wake-clips.json";
const INDEX_FILE: &str = "wake-clips.index.jsonl";
const MAX_CLIP_SAMPLES: usize = 3 * 16_000;
const MAX_INDEX_ENTRY_BYTES: usize = 4096;
static NEXT_CLIP: AtomicU64 = AtomicU64::new(0);
static INDEX_LOCK: Mutex<()> = Mutex::new(());

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IndexEntry {
    metadata: ClipMetadata,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipSettings {
    pub enabled: bool,
    pub directory: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClipLabel {
    Unreviewed,
    TrueWake,
    FalseWake,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipMetadata {
    pub id: String,
    pub profile: String,
    pub model_file: String,
    pub model_revision: String,
    pub created_ms: u64,
    pub confidence: f32,
    pub label: ClipLabel,
    pub sample_rate_hz: u32,
    pub sample_count: usize,
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(crate::application_config_path()?.with_file_name(SETTINGS_FILE))
}

pub(crate) fn load_settings() -> Result<ClipSettings, String> {
    let path = settings_path()?;
    let default_directory = crate::application_config_path()?.with_file_name("wake-clips");
    load_settings_from(&path, &default_directory)
}

fn load_settings_from(path: &Path, default_directory: &Path) -> Result<ClipSettings, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(ClipSettings {
                enabled: true,
                directory: default_directory.to_string_lossy().into_owned(),
            });
        }
        Err(error) => return Err(format!("failed to read wake clip settings: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err(String::from("wake clip settings file is invalid"));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read wake clip settings: {error}"))?;
    let settings: ClipSettings = serde_json::from_slice(&bytes)
        .map_err(|_| String::from("wake clip settings are invalid"))?;
    validate_directory(Path::new(&settings.directory))?;
    Ok(settings)
}

pub(crate) fn set_settings(enabled: bool, directory: &str) -> Result<ClipSettings, String> {
    set_settings_at(&settings_path()?, enabled, directory)
}

fn set_settings_at(path: &Path, enabled: bool, directory: &str) -> Result<ClipSettings, String> {
    let root = Path::new(directory);
    validate_directory(root)?;
    if enabled {
        ensure_directory(root)?;
    }
    let settings = ClipSettings {
        enabled,
        directory: root.to_string_lossy().into_owned(),
    };
    let parent = path.parent().ok_or("wake clip settings have no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create settings directory: {error}"))?;
    if fs::symlink_metadata(path).is_ok_and(|entry| entry.file_type().is_symlink()) {
        return Err(String::from("wake clip settings path is a symlink"));
    }
    crate::atomic_replace_state_file(
        path,
        &serde_json::to_vec(&settings).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("failed to save wake clip settings: {error}"))?;
    Ok(settings)
}

fn validate_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.to_string_lossy().len() > 2048
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(String::from(
            "wake clip directory must be an absolute path without '..'",
        ));
    }
    #[cfg(windows)]
    if path.components().any(|part| matches!(part,
        std::path::Component::Prefix(prefix)
            if matches!(prefix.kind(), std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..))
    )) {
        return Err(String::from("wake clips must be saved to a local drive"));
    }
    if path.ancestors().any(|parent| parent.join(".git").exists()) {
        return Err(String::from(
            "wake clips cannot be stored inside a source repository",
        ));
    }
    if let Some(existing) = path.ancestors().find(|ancestor| ancestor.exists()) {
        let resolved = fs::canonicalize(existing).map_err(|error| error.to_string())?;
        if resolved
            .ancestors()
            .any(|parent| parent.join(".git").exists())
        {
            return Err(String::from(
                "wake clips cannot resolve inside a source repository",
            ));
        }
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    validate_directory(path)?;
    if fs::symlink_metadata(path).is_ok_and(|entry| entry.file_type().is_symlink()) {
        return Err(String::from("wake clip directory must not be a symlink"));
    }
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create wake clip directory: {error}"))?;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(String::from("wake clip directory must not be a symlink"));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect wake clip directory: {error}"))?;
    Ok(())
}

fn profile_from_model(model_file: &str, revision: &str) -> String {
    let stem = Path::new(model_file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("model");
    let clean: String = stem
        .chars()
        .take(63)
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "{}-{revision}",
        if clean.is_empty() { "model" } else { &clean }
    )
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn clip_path(root: &Path, profile: &str, id: &str, extension: &str) -> Result<PathBuf, String> {
    if !valid_identifier(profile) || !valid_identifier(id) {
        return Err(String::from("invalid wake clip identifier"));
    }
    Ok(root.join(profile).join(format!("{id}.{extension}")))
}

fn validate_clip_folder(root: &Path, profile: &str) -> Result<(), String> {
    validate_directory(root)?;
    if !valid_identifier(profile) {
        return Err(String::from("invalid wake clip profile"));
    }
    for path in [root.to_path_buf(), root.join(profile)] {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(String::from("wake clip folder must be a regular directory"));
        }
    }
    Ok(())
}

pub(crate) fn save_clip(
    root: &Path,
    model_file: &str,
    model_revision: &str,
    samples: &[f32],
    confidence: f32,
) -> Result<ClipMetadata, String> {
    if model_revision.len() != 16
        || !model_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        || samples.is_empty()
        || samples.len() > MAX_CLIP_SAMPLES
        || samples.iter().any(|sample| !sample.is_finite())
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
    {
        return Err(String::from("wake clip audio or confidence is invalid"));
    }
    let profile = profile_from_model(model_file, model_revision);
    ensure_directory(root)?;
    ensure_directory(&root.join(&profile))?;
    ensure_index(root)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    let created_ms = now.as_millis().min(u128::from(u64::MAX)) as u64;
    let id = format!(
        "{}-{}",
        now.as_nanos(),
        NEXT_CLIP.fetch_add(1, Ordering::Relaxed)
    );
    let wav_path = clip_path(root, &profile, &id, "wav")?;
    let metadata_path = clip_path(root, &profile, &id, "json")?;
    let metadata = ClipMetadata {
        id,
        profile,
        model_file: model_file.to_owned(),
        model_revision: model_revision.to_owned(),
        created_ms,
        confidence,
        label: ClipLabel::Unreviewed,
        sample_rate_hz: 16_000,
        sample_count: samples.len(),
    };
    if serde_json::to_vec(&metadata)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_INDEX_ENTRY_BYTES - 32
    {
        return Err(String::from(
            "wake model filename is too long for clip metadata",
        ));
    }
    append_index(root, &metadata)?;
    let data_len = u32::try_from(samples.len() * 2).map_err(|error| error.to_string())?;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let write = (|| -> Result<(), String> {
        let mut file = options
            .open(&wav_path)
            .map_err(|error| format!("cannot create wake clip: {error}"))?;
        file.write_all(&wav)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("cannot write wake clip: {error}"))?;
        drop(file);
        let bytes = serde_json::to_vec(&metadata).map_err(|error| error.to_string())?;
        crate::atomic_replace_state_file(&metadata_path, &bytes)
            .map_err(|error| format!("cannot write wake clip metadata: {error}"))
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&metadata_path);
        let _ = fs::remove_file(&wav_path);
        let _ = compact_index(root, &metadata.profile, &metadata.id);
        return Err(error);
    }
    Ok(metadata)
}

fn append_index(root: &Path, metadata: &ClipMetadata) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|_| String::from("wake clip index lock is poisoned"))?;
    ensure_index_locked(root)?;
    let path = root.join(INDEX_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|entry| entry.file_type().is_symlink()) {
        return Err(String::from("wake clip index path is a symlink"));
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("cannot open wake clip index: {error}"))?;
    let entry = IndexEntry {
        metadata: metadata.clone(),
    };
    let mut line = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
    line.push(b'\n');
    let before = file.metadata().map_err(|error| error.to_string())?.len();
    if let Err(error) = file.write_all(&line).and_then(|()| file.sync_all()) {
        let _ = file.set_len(before);
        return Err(format!("cannot write wake clip index: {error}"));
    }
    Ok(())
}

fn ensure_index(root: &Path) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|_| String::from("wake clip index lock is poisoned"))?;
    ensure_index_locked(root)?;
    Ok(())
}

fn ensure_index_locked(root: &Path) -> Result<bool, String> {
    let path = root.join(INDEX_FILE);
    let info = match fs::symlink_metadata(&path) {
        Ok(info) => info,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            rebuild_index(root, &path)?;
            return Ok(true);
        }
        Err(error) => return Err(error.to_string()),
    };
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(String::from("wake clip index is not a regular file"));
    }
    if info.len() == 0 {
        rebuild_index(root, &path)?;
        return Ok(true);
    }
    let mut file = fs::File::open(&path).map_err(|error| error.to_string())?;
    file.seek(SeekFrom::End(-1))
        .map_err(|error| error.to_string())?;
    let mut last = [0];
    file.read_exact(&mut last)
        .map_err(|error| error.to_string())?;
    if last[0] != b'\n' {
        rebuild_index(root, &path)?;
        return Ok(true);
    }
    Ok(false)
}

fn rebuild_index(root: &Path, path: &Path) -> Result<(), String> {
    use std::io::BufRead as _;
    let mut clips = std::collections::BTreeMap::new();
    if let Ok(file) = fs::File::open(path) {
        let mut reader = std::io::BufReader::new(file);
        let mut line = Vec::new();
        while reader
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?
            != 0
        {
            let decoded = serde_json::from_slice::<IndexEntry>(&line);
            line.clear();
            if let Ok(entry) = decoded {
                let clip = entry.metadata;
                if clip.sample_count == 0
                    || clip.sample_count > MAX_CLIP_SAMPLES
                    || validate_clip_folder(root, &clip.profile).is_err()
                {
                    continue;
                }
                if let Ok(wav) = clip_path(root, &clip.profile, &clip.id, "wav") {
                    if fs::symlink_metadata(wav).is_ok_and(|info| {
                        info.is_file()
                            && !info.file_type().is_symlink()
                            && info.len() == 44 + clip.sample_count as u64 * 2
                    }) {
                        clips.insert((clip.profile.clone(), clip.id.clone()), clip);
                    }
                }
            }
        }
    }
    for folder in fs::read_dir(root).map_err(|error| error.to_string())? {
        let folder = folder.map_err(|error| error.to_string())?;
        if !folder
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let profile = folder.file_name().to_string_lossy().into_owned();
        if !valid_identifier(&profile) {
            continue;
        }
        for item in fs::read_dir(folder.path()).map_err(|error| error.to_string())? {
            let item = item.map_err(|error| error.to_string())?;
            if !item
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
                || item.path().extension().and_then(|ext| ext.to_str()) != Some("json")
            {
                continue;
            }
            let id = item
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if let Ok(metadata) = read_metadata(root, &profile, &id) {
                if let Ok(wav) = clip_path(root, &profile, &id, "wav") {
                    if fs::symlink_metadata(wav)
                        .is_ok_and(|info| info.is_file() && !info.file_type().is_symlink())
                    {
                        clips.insert((metadata.profile.clone(), metadata.id.clone()), metadata);
                    }
                }
            }
        }
    }
    let mut clips = clips.into_values().collect::<Vec<_>>();
    clips.sort_by(|left, right| {
        left.created_ms
            .cmp(&right.created_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut bytes = Vec::new();
    for clip in clips {
        serde_json::to_writer(&mut bytes, &IndexEntry { metadata: clip })
            .map_err(|error| error.to_string())?;
        bytes.push(b'\n');
    }
    crate::atomic_replace_state_file(path, &bytes)
        .map_err(|error| format!("cannot rebuild wake clip index: {error}"))
}

fn read_metadata(root: &Path, profile: &str, id: &str) -> Result<ClipMetadata, String> {
    validate_clip_folder(root, profile)?;
    let path = clip_path(root, profile, id, "json")?;
    let info = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !info.is_file() || info.file_type().is_symlink() || info.len() > 4096 {
        return Err(String::from("wake clip metadata is invalid"));
    }
    let metadata: ClipMetadata =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|_| String::from("wake clip metadata is malformed"))?;
    if metadata.id != id || metadata.profile != profile {
        return Err(String::from("wake clip metadata does not match its path"));
    }
    Ok(metadata)
}

fn indexed_metadata<F>(
    root: &Path,
    entry: &IndexEntry,
    load: &mut F,
) -> Result<Option<ClipMetadata>, String>
where
    F: FnMut(&Path, &str, &str) -> Result<ClipMetadata, String>,
{
    let expected = &entry.metadata;
    if expected.sample_count == 0 || expected.sample_count > MAX_CLIP_SAMPLES {
        return Ok(None);
    }
    if validate_clip_folder(root, &expected.profile).is_err() {
        return Ok(None);
    }
    let wav = match clip_path(root, &expected.profile, &expected.id, "wav") {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !fs::symlink_metadata(wav).is_ok_and(|info| {
        info.is_file()
            && !info.file_type().is_symlink()
            && info.len() == 44 + expected.sample_count as u64 * 2
    }) {
        return Ok(None);
    }
    if let Ok(metadata) = load(root, &expected.profile, &expected.id) {
        return Ok(Some(metadata));
    }
    let path = clip_path(root, &expected.profile, &expected.id, "json")?;
    if !matches!(fs::symlink_metadata(&path), Err(ref error) if error.kind() == ErrorKind::NotFound)
    {
        return Ok(None);
    }
    crate::atomic_replace_state_file(
        &path,
        &serde_json::to_vec(expected).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("cannot recover wake clip metadata: {error}"))?;
    load(root, &expected.profile, &expected.id).map(Some)
}

pub(crate) fn list_clips(root: &Path, offset: usize) -> Result<Vec<ClipMetadata>, String> {
    list_clips_with(root, offset, read_metadata)
}

fn list_clips_with<F>(root: &Path, offset: usize, mut load: F) -> Result<Vec<ClipMetadata>, String>
where
    F: FnMut(&Path, &str, &str) -> Result<ClipMetadata, String>,
{
    validate_directory(root)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let root_metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(String::from("wake clip folder must be a regular directory"));
    }
    ensure_index(root)?;
    let index_path = root.join(INDEX_FILE);
    if fs::symlink_metadata(&index_path).is_ok_and(|entry| entry.file_type().is_symlink()) {
        return Err(String::from("wake clip index is not a regular file"));
    }
    let mut file = match OpenOptions::new().read(true).open(index_path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read wake clip index: {error}")),
    };
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(String::from("wake clip index is not a regular file"));
    }
    let mut position = file.metadata().map_err(|error| error.to_string())?.len();
    let mut reversed = Vec::new();
    let mut clips = Vec::new();
    let mut available = 0_usize;
    while position > 0 && clips.len() < 100 {
        let length = position.min(8192) as usize;
        position -= length as u64;
        file.seek(SeekFrom::Start(position))
            .map_err(|error| error.to_string())?;
        let mut block = vec![0; length];
        file.read_exact(&mut block)
            .map_err(|error| error.to_string())?;
        for &byte in block.iter().rev() {
            if byte == b'\n' {
                if reversed.is_empty() {
                    continue;
                }
                reversed.reverse();
                if let Ok(entry) = serde_json::from_slice::<IndexEntry>(&reversed) {
                    if let Some(metadata) = indexed_metadata(root, &entry, &mut load)? {
                        if available >= offset {
                            clips.push(metadata);
                        }
                        available += 1;
                    }
                }
                reversed.clear();
                if clips.len() == 100 {
                    break;
                }
            } else if reversed.len() < MAX_INDEX_ENTRY_BYTES {
                reversed.push(byte);
            } else {
                return Err(String::from("wake clip index entry is too long"));
            }
        }
    }
    if !reversed.is_empty() && clips.len() < 100 {
        reversed.reverse();
        if let Ok(entry) = serde_json::from_slice::<IndexEntry>(&reversed) {
            if let Some(metadata) = indexed_metadata(root, &entry, &mut load)? {
                if available >= offset {
                    clips.push(metadata);
                }
            }
        }
    }
    Ok(clips)
}

pub(crate) fn set_label(
    root: &Path,
    profile: &str,
    id: &str,
    label: ClipLabel,
) -> Result<ClipMetadata, String> {
    let mut metadata = read_metadata(root, profile, id)?;
    metadata.label = label;
    let bytes = serde_json::to_vec(&metadata).map_err(|error| error.to_string())?;
    crate::atomic_replace_state_file(&clip_path(root, profile, id, "json")?, &bytes)
        .map_err(|error| format!("cannot update wake clip label: {error}"))?;
    Ok(metadata)
}

pub(crate) fn read_clip(root: &Path, profile: &str, id: &str) -> Result<Vec<u8>, String> {
    let metadata = read_metadata(root, profile, id)?;
    let path = clip_path(root, profile, id, "wav")?;
    let info = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !info.is_file()
        || info.file_type().is_symlink()
        || metadata.sample_count > MAX_CLIP_SAMPLES
        || info.len() != 44 + (metadata.sample_count * 2) as u64
        || info.len() > 44 + MAX_CLIP_SAMPLES as u64 * 2
    {
        return Err(String::from("wake clip WAV is invalid"));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(String::from("wake clip WAV header is invalid"));
    }
    Ok(bytes)
}

pub(crate) fn clip_data_url(root: &Path, profile: &str, id: &str) -> Result<String, String> {
    Ok(format!(
        "data:audio/wav;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(read_clip(root, profile, id)?)
    ))
}

pub(crate) fn delete_clip(root: &Path, profile: &str, id: &str) -> Result<(), String> {
    read_metadata(root, profile, id)?;
    let wav = clip_path(root, profile, id, "wav")?;
    let info = fs::symlink_metadata(&wav).map_err(|error| error.to_string())?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(String::from("wake clip is not a regular file"));
    }
    fs::remove_file(wav).map_err(|error| format!("cannot delete wake clip: {error}"))?;
    fs::remove_file(clip_path(root, profile, id, "json")?)
        .map_err(|error| format!("cannot delete wake clip metadata: {error}"))?;
    compact_index(root, profile, id)
}

fn compact_index(root: &Path, profile: &str, id: &str) -> Result<(), String> {
    use std::io::BufRead as _;
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|_| String::from("wake clip index lock is poisoned"))?;
    if ensure_index_locked(root)? {
        return Ok(());
    }
    let path = root.join(INDEX_FILE);
    let original = fs::File::open(&path).map_err(|error| error.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(|error| error.to_string())?;
    for line in std::io::BufReader::new(original).lines() {
        let line = line.map_err(|error| error.to_string())?;
        if let Ok(entry) = serde_json::from_str::<IndexEntry>(&line) {
            if entry.metadata.profile == profile && entry.metadata.id == id {
                continue;
            }
        }
        temporary
            .write_all(line.as_bytes())
            .and_then(|()| temporary.write_all(b"\n"))
            .map_err(|error| error.to_string())?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| format!("cannot compact wake clip index: {}", error.error))?;
    Ok(())
}
