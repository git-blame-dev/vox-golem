use super::{safe_opaque, AuthError, MAX_BODY};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const MANAGED_VERSION: u8 = 1;

// Intentionally not Debug: never put credential material into diagnostics.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManagedFile {
    pub voxgolem_chatgpt: u8,
    pub openai: Credential,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Credential {
    pub access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    pub expires: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

pub(super) fn parse(bytes: &[u8]) -> Result<ManagedFile, AuthError> {
    let file: ManagedFile = serde_json::from_slice(bytes).map_err(|_| AuthError::Storage)?;
    if file.voxgolem_chatgpt != MANAGED_VERSION
        || !safe_opaque(&file.openai.access, 24_000)
        || file
            .openai
            .refresh
            .as_ref()
            .is_some_and(|refresh| !safe_opaque(refresh, 24_000))
        || file.openai.expires > 9_007_199_254_740_991
        || file
            .openai
            .account_id
            .as_ref()
            .is_some_and(|account| !safe_opaque(account, 256))
    {
        return Err(AuthError::Storage);
    }
    Ok(file)
}

pub(super) fn tombstone(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|value| {
            value
                .get("voxgolemChatgpt")
                .and_then(|value| value.as_u64())
                == Some(u64::from(MANAGED_VERSION))
                && value.get("signedOut").and_then(|value| value.as_bool()) == Some(true)
                && value.get("openai").is_none()
        })
}

pub(super) fn owned(bytes: &[u8]) -> bool {
    parse(bytes).is_ok() || tombstone(bytes)
}

pub(super) fn marked(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Marker {
        #[serde(rename = "voxgolemChatgpt")]
        version: u8,
    }
    // Skip credential strings instead of allocating another complete JSON tree
    // merely to enforce the managed-token endpoint boundary.
    serde_json::from_slice::<Marker>(bytes).is_ok_and(|marker| marker.version == MANAGED_VERSION)
}

/// Legacy imported symlinks remain readable by the original provider parser,
/// but are never a candidate for managed refresh, locks, or publication.
pub(super) fn refresh_file(path: &Path) -> Result<Option<Vec<u8>>, AuthError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) {
        let metadata = fs::metadata(path).map_err(|_| AuthError::Storage)?;
        if !metadata.is_file() || metadata.len() > MAX_BODY as u64 {
            return Err(AuthError::Storage);
        }
        let bytes = read_contents(path)?;
        if marked(&bytes) {
            return Err(AuthError::Storage);
        }
        return Ok(None);
    }
    read_file(path)
}

pub(super) fn read_file(path: &Path) -> Result<Option<Vec<u8>>, AuthError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AuthError::Storage),
    };
    if !metadata.is_file() || metadata.len() > MAX_BODY as u64 {
        return Err(AuthError::Storage);
    }
    read_contents(path).map(Some)
}

fn read_contents(path: &Path) -> Result<Vec<u8>, AuthError> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| AuthError::Storage)?
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AuthError::Storage)?;
    if bytes.len() > MAX_BODY {
        return Err(AuthError::Storage);
    }
    Ok(bytes)
}

fn private_parent(path: &Path) -> Result<&Path, AuthError> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(AuthError::Storage)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent).map_err(|_| AuthError::Storage)?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| AuthError::Storage)?;
    if !metadata.is_dir() {
        return Err(AuthError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(AuthError::Storage);
        }
    }
    Ok(parent)
}

/// Call only while holding the path owner's state lock and after fencing the
/// original file contents. Parent directory must be owned by the application.
pub(super) fn write_file(path: &Path, file: &ManagedFile) -> Result<(), AuthError> {
    let bytes = serde_json::to_vec(file).map_err(|_| AuthError::Storage)?;
    write_bytes(path, &bytes)
}

pub(super) fn write_tombstone(path: &Path) -> Result<(), AuthError> {
    static FENCE: AtomicU64 = AtomicU64::new(0);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| AuthError::Storage)?
        .as_nanos();
    // A new durable fence on *every* explicit sign-out prevents ABA: an older
    // process's pending login must not accept a later identical signed-out file.
    let fence = format!(
        "{}-{epoch}-{}",
        std::process::id(),
        FENCE.fetch_add(1, Ordering::Relaxed)
    );
    let bytes = serde_json::to_vec(
        &serde_json::json!({"voxgolemChatgpt":MANAGED_VERSION,"signedOut":true,"fence":fence}),
    )
    .map_err(|_| AuthError::Storage)?;
    write_bytes(path, &bytes)
}

/// The returned File owns the OS lock until drop. Never unlink lock files:
/// replacing their inode would permit a second process to lock a different file.
pub(super) fn try_lock(path: &Path) -> Result<Option<fs::File>, AuthError> {
    let parent = private_parent(path)?;
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().ok_or(AuthError::Storage)?);
    name.push(".lock");
    let path = parent.join(name);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file() {
            return Err(AuthError::Storage);
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| AuthError::Storage)?;
    Ok(try_lock_exclusive(&file)?.then_some(file))
}

/// `Ok(true)` when acquired, `Ok(false)` when another owner holds it. std's `File::try_lock`
/// is unsupported on Android, so Unix (including Android) uses `flock(2)` directly.
#[cfg(unix)]
fn try_lock_exclusive(file: &fs::File) -> Result<bool, AuthError> {
    use rustix::fs::{flock, FlockOperation};
    match flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(_) => Err(AuthError::Storage),
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(file: &fs::File) -> Result<bool, AuthError> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(_)) => Err(AuthError::Storage),
    }
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), AuthError> {
    let parent = private_parent(path)?;
    if bytes.len() > MAX_BODY {
        return Err(AuthError::Storage);
    }
    static TEMP: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".chatgpt-{}-{}.tmp",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary).map_err(|_| AuthError::Storage)?;
    let result = (|| {
        output.write_all(bytes).map_err(|_| AuthError::Storage)?;
        output.sync_all().map_err(|_| AuthError::Storage)?;
        drop(output);
        fs::rename(&temporary, path).map_err(|_| AuthError::Storage)?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| AuthError::Storage)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(super) fn normalized_path(path: &Path) -> Result<PathBuf, AuthError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| AuthError::Storage)?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    let name = normalized.file_name().ok_or(AuthError::Storage)?.to_owned();
    let mut ancestor = normalized.parent().ok_or(AuthError::Storage)?;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        missing.push(ancestor.file_name().ok_or(AuthError::Storage)?.to_owned());
        ancestor = ancestor.parent().ok_or(AuthError::Storage)?;
    }
    let mut canonical = ancestor.canonicalize().map_err(|_| AuthError::Storage)?;
    for part in missing.into_iter().rev() {
        canonical.push(part);
    }
    canonical.push(name);
    Ok(canonical)
}
