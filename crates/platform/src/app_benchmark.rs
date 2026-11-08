//! Fail-closed safety boundary for the opt-in application benchmark only.
use crate::custom_openai::{CustomOpenAiConfig, CustomOpenAiError, CustomOpenAiModel};
#[cfg(unix)]
use serde::Deserialize;
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Read,
};
use std::{
    path::Path,
    sync::{LazyLock, Mutex},
};

type Result<T> = std::result::Result<T, CustomOpenAiError>;
fn blocked() -> CustomOpenAiError {
    CustomOpenAiError::InvalidConfig
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Counts {
    pub generation_attempts: u64,
    pub spent: u64,
    pub remaining: u64,
}
#[derive(Default)]
struct State {
    counts: Counts,
    allowance: Option<Result<u64>>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

pub fn get_counts() -> Counts {
    STATE.lock().map(|s| s.counts).unwrap_or_default()
}

/// Claim the pre-reserved allowance before app/voice admission, never on the
/// measured first generation's critical path. A failed claim stays failed.
pub fn initialize() -> Result<()> {
    if mode()? {
        STATE.lock().map_err(|_| blocked())?.prepare(|| {
            let path = std::env::var_os("VOXGOLEM_BENCH_PERMITS").ok_or_else(blocked)?;
            claim(Path::new(&path))
        })?;
    }
    Ok(())
}

fn mode() -> Result<bool> {
    parse_mode(
        std::env::var("VOXGOLEM_BENCH_MODE").ok().as_deref(),
        std::env::var_os("VOXGOLEM_BENCH_PERMITS").is_some(),
        std::env::var_os("VOXGOLEM_BENCH_MCP_ENDPOINT").is_some(),
    )
}

fn parse_mode(value: Option<&str>, permits: bool, endpoint: bool) -> Result<bool> {
    match value {
        Some("offline") if !permits => Ok(false),
        Some("live") if !endpoint => Ok(true),
        _ => Err(blocked()),
    }
}

fn loopback(value: &str, mcp: bool) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| blocked())?;
    let numeric = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if !numeric
        || url.scheme() != "http"
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (mcp && url.path() != "/mcp")
    {
        return Err(blocked());
    }
    Ok(())
}

pub(crate) fn validate_generation(config: &CustomOpenAiConfig) -> Result<()> {
    validate_generation_mode(config, mode()?)
}

fn validate_generation_mode(config: &CustomOpenAiConfig, live: bool) -> Result<()> {
    if live {
        if config.model != CustomOpenAiModel::LunaNone
            || !config.endpoint.starts_with("https:")
            || !crate::custom_openai::valid_endpoint(&config.endpoint)
        {
            return Err(blocked());
        }
        Ok(())
    } else {
        loopback(&config.endpoint, false)
    }
}

/// Resolve only the MCP transport address; public tool URL validation is unchanged.
pub(crate) fn mcp_endpoint(builtin: &str) -> Result<String> {
    if mode()? {
        return Ok(builtin.to_owned());
    }
    let endpoint = std::env::var("VOXGOLEM_BENCH_MCP_ENDPOINT").map_err(|_| blocked())?;
    loopback(&endpoint, true)?;
    Ok(endpoint)
}

#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    schema: u64,
    allowance: u64,
}

#[cfg(not(any(unix, all(windows, feature = "desktop"))))]
fn claim(_path: &Path) -> Result<u64> {
    // Private-file permissions and directory durability are only verified on Unix.
    Err(blocked())
}

#[cfg(all(windows, feature = "desktop"))]
fn claim(path: &Path) -> Result<u64> {
    windows_claim::claim(path)
}

// The controller and native claimant use exactly the same WSL authority code.
// Claim uses no source-checkout imports; paths.py is embedded as well.
#[cfg(all(windows, feature = "desktop"))]
#[allow(unsafe_code)]
mod windows_claim {
    use super::{blocked, Result};
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command, Stdio};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::*;
    use windows::Win32::Storage::FileSystem::*;
    use windows::Win32::System::SystemInformation::{
        GetSystemDirectoryW, GetSystemWindowsDirectoryW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Handoff {
        schema: u64,
        distribution: String,
        source_root: String,
        grant: String,
        windows_run: PathBuf,
        binding: serde_json::Value,
    }

    fn open(path: &Path, directory: bool) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(if directory {
                FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0
            } else {
                FILE_SHARE_READ.0
            })
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(path)
            .map_err(|_| blocked())?;
        // SAFETY: the File owns a valid handle; the output structure is valid
        // for this call. Retaining File also prevents delete/rename sharing.
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| blocked())?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
        {
            return Err(blocked());
        }
        Ok(file)
    }

    fn os_directory(query: unsafe fn(Option<&mut [u16]>) -> u32) -> Result<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: the maintained binding supplies the slice's actual capacity.
        // Zero/truncated results are rejected before decoding the bounded path.
        let length = unsafe { query(Some(&mut buffer)) } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(blocked());
        }
        let path = PathBuf::from(OsString::from_wide(&buffer[..length]));
        if !path.is_absolute() {
            return Err(blocked());
        }
        Ok(path)
    }

    struct SystemLauncher {
        path: PathBuf,
        windows_root: PathBuf,
        _held: Vec<File>,
    }

    impl SystemLauncher {
        fn command(&self) -> Command {
            let mut command = Command::new(&self.path);
            // No caller PATH, WSLENV, loader overrides or private app environment
            // may change the canonical authority program executed inside WSL.
            command
                .env_clear()
                .env("SystemRoot", &self.windows_root)
                .env("WINDIR", &self.windows_root);
            command
        }
    }

    fn retain_wsl_launcher(candidate: Option<&Path>) -> Result<SystemLauncher> {
        // These are OS queries, never caller-controlled SystemRoot/WINDIR/PATH.
        // GetSystemWindowsDirectoryW also avoids per-user Terminal Services roots.
        let windows_root = os_directory(GetSystemWindowsDirectoryW)?;
        let system = os_directory(GetSystemDirectoryW)?;
        let expected = system.join("wsl.exe");
        if candidate.is_some_and(|path| path != expected) {
            return Err(blocked());
        }
        let mut held = Vec::new();
        for ancestor in expected
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            held.push(open(ancestor, true)?);
        }
        let windows = windows_root.canonicalize().map_err(|_| blocked())?;
        let system = system.canonicalize().map_err(|_| blocked())?;
        if system == windows || !system.starts_with(&windows) {
            return Err(blocked());
        }
        let file = open(&expected, false)?;
        let canonical = expected.canonicalize().map_err(|_| blocked())?;
        let mut buffer = vec![0u16; 32768];
        // SAFETY: File retains the valid handle, the mutable output slice is
        // bounded, and the opened file and ancestors deny delete/rename sharing.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                HANDLE(file.as_raw_handle()),
                &mut buffer,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        } as usize;
        if length == 0
            || length >= buffer.len()
            || OsString::from_wide(&buffer[..length]) != canonical.as_os_str()
        {
            return Err(blocked());
        }
        held.push(file);
        Ok(SystemLauncher {
            path: canonical,
            windows_root,
            _held: held,
        })
    }

    fn private(file: &File, protected: bool) -> Result<()> {
        // SAFETY: token buffers are pointer-aligned and bounded, SID/ACE pointers
        // remain valid until their owning token buffer/security descriptor drops.
        // Every Win32 allocation/handle is released on both success and error.
        unsafe {
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info)
                .map_err(|_| blocked())?;
            if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 && info.nNumberOfLinks != 1 {
                return Err(blocked());
            }
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
                .map_err(|_| blocked())?;
            let result = (|| {
                let mut length = 0;
                let _ = GetTokenInformation(token, TokenUser, None, 0, &mut length);
                if length == 0 || length > 65536 {
                    return Err(blocked());
                }
                let mut buffer =
                    vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(buffer.as_mut_ptr().cast()),
                    length,
                    &mut length,
                )
                .map_err(|_| blocked())?;
                let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
                let mut owner = PSID::default();
                let mut acl = std::ptr::null_mut();
                let mut descriptor = PSECURITY_DESCRIPTOR::default();
                GetSecurityInfo(
                    HANDLE(file.as_raw_handle()),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    Some(&mut owner),
                    None,
                    Some(&mut acl),
                    None,
                    Some(&mut descriptor),
                )
                .ok()
                .map_err(|_| blocked())?;
                let checked = (|| {
                    EqualSid(owner, user.User.Sid).map_err(|_| blocked())?;
                    if acl.is_null() || (*acl).AceCount != 2 {
                        return Err(blocked());
                    }
                    let mut control = 0;
                    let mut revision = 0;
                    GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
                        .map_err(|_| blocked())?;
                    if protected && control & SE_DACL_PROTECTED.0 == 0 {
                        return Err(blocked());
                    }
                    let mut user_seen = false;
                    let mut system_seen = false;
                    for index in 0..2 {
                        let mut ace = std::ptr::null_mut();
                        GetAce(acl, index, &mut ace).map_err(|_| blocked())?;
                        let header = &*ace.cast::<ACE_HEADER>();
                        if header.AceType != 0
                            || usize::from(header.AceSize)
                                < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                            || header.AceFlags & !0x13 != 0
                        {
                            return Err(blocked());
                        }
                        let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                        if allowed.Header.AceType != 0 || allowed.Mask != FILE_ALL_ACCESS.0 {
                            return Err(blocked());
                        }
                        let sid = PSID((&raw const allowed.SidStart).cast_mut().cast());
                        if EqualSid(sid, user.User.Sid).is_ok() && !user_seen {
                            user_seen = true;
                        } else if IsWellKnownSid(sid, WinLocalSystemSid).as_bool() && !system_seen {
                            system_seen = true;
                        } else {
                            return Err(blocked());
                        }
                    }
                    if user_seen && system_seen {
                        Ok(())
                    } else {
                        Err(blocked())
                    }
                })();
                let _ = LocalFree(Some(HLOCAL(descriptor.0)));
                checked
            })();
            let _ = CloseHandle(token);
            result
        }
    }

    pub(super) fn claim(path: &Path) -> Result<u64> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::to_str)
            .ok_or_else(blocked)?;
        let distribution = option_env!("VOXGOLEM_BENCH_WSL_DISTRIBUTION")
            .or(option_env!("WSL_DISTRO_NAME"))
            .ok_or_else(blocked)?;
        if std::env::var("VOXGOLEM_BENCH_WINDOWS_PROFILE").as_deref() != Ok("live-qualified-v1")
            || std::env::var("VOXGOLEM_BENCH_ENTRY").as_deref()
                != Ok("typed-speech-qualified-windows")
        {
            return Err(blocked());
        }
        if !path.is_absolute()
            || !matches!(path.components().next(), Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(blocked());
        }
        let root = PathBuf::from(std::env::var_os("VOXGOLEM_BENCH_ROOT").ok_or_else(blocked)?);
        if path.parent() != Some(root.as_path())
            || path.file_name().and_then(|s| s.to_str()) != Some("reservation.json")
        {
            return Err(blocked());
        }
        // Retain every directory handle through the claim, including ancestors
        // outside the private run. No component can be replaced underneath it.
        let mut held = Vec::new();
        for ancestor in root.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            held.push(open(ancestor, true)?);
        }
        private(held.first().ok_or_else(blocked)?, true)?;
        let file = open(path, false)?;
        private(&file, false)?;
        let mut bytes = Vec::new();
        (&file)
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| blocked())?;
        if bytes.len() > 16384 {
            return Err(blocked());
        }
        let handoff: Handoff = serde_json::from_slice(&bytes).map_err(|_| blocked())?;
        let resolved_root = root.canonicalize().map_err(|_| blocked())?;
        let resolved_handoff = handoff.windows_run.canonicalize().map_err(|_| blocked())?;
        if handoff.schema != 2
            || handoff.distribution != distribution
            || handoff.source_root != source
            || handoff.distribution.is_empty()
            || handoff.distribution.len() > 128
            || !handoff
                .distribution
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || !handoff.source_root.starts_with('/')
            || handoff.source_root.len() > 4096
            || handoff.grant.len() != 32
            || !handoff
                .grant
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || !handoff.windows_run.is_absolute()
            || resolved_handoff != resolved_root
            || handoff
                .binding
                .get("profile")
                .and_then(|value| value.as_str())
                != Some("windows-neural-live-qualified-v1")
        {
            return Err(blocked());
        }
        let marker = root.join("reservation.json.claimed");
        let claimed = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_WRITE_THROUGH.0)
            .open(&marker)
            .map_err(|_| blocked())?;
        private(&claimed, false)?;
        claimed.sync_all().map_err(|_| blocked())?;
        let wsl = retain_wsl_launcher(None)?;
        let script = concat!(
            include_str!("../../../tools/app-bench/paths.py"),
            "\n",
            include_str!("../../../tools/app-bench/windows_assignment.py")
        );
        let executable = std::env::current_exe().map_err(|_| blocked())?;
        let executable = executable
            .to_str()
            .ok_or_else(blocked)?
            .trim_start_matches(r"\\?\");
        let binding = serde_json::to_string(&handoff.binding).map_err(|_| blocked())?;
        let mut child = wsl
            .command()
            .args([
                "--distribution",
                &handoff.distribution,
                "--exec",
                "/usr/bin/python3",
                "-I",
                "-c",
                script,
                "claim",
                &handoff.source_root,
                &handoff.grant,
                executable,
                handoff.windows_run.to_str().ok_or_else(blocked)?,
                &binding,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| blocked())?;
        let started = std::time::Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed().as_secs() < 30 => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(blocked());
                }
            }
        };
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(blocked)?
            .take(1025)
            .read_to_end(&mut output)
            .map_err(|_| blocked())?;
        let receipt: serde_json::Value = serde_json::from_slice(&output).map_err(|_| blocked())?;
        if !status.success()
            || output.len() > 1024
            || receipt != serde_json::json!({"allowance":12})
        {
            return Err(blocked());
        }
        Ok(12)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn os_launcher_rejects_spoofed_system_root_and_fake_allowance_receipt() {
            const ROLE: &str = "VOXGOLEM_SYNTHETIC_LAUNCHER_SPOOF";
            const TEST: &str = "app_benchmark::windows_claim::tests::os_launcher_rejects_spoofed_system_root_and_fake_allowance_receipt";
            if std::env::var_os(ROLE).is_some() {
                let fake =
                    PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/wsl.exe");
                let receipt = br#"{"allowance":12}"#;
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(receipt).unwrap(),
                    serde_json::json!({"allowance":12})
                );
                assert!(fake.is_file());
                // A real PE at the spoofed path and a syntactically valid reply
                // cannot substitute for the OS-derived authority executable.
                assert!(retain_wsl_launcher(Some(&fake)).is_err());
                let expected = os_directory(GetSystemDirectoryW).unwrap().join("wsl.exe");
                assert_ne!(expected, fake);
                if expected.is_file() {
                    let launcher = retain_wsl_launcher(None).unwrap();
                    assert_eq!(launcher.path, expected.canonicalize().unwrap());
                    assert!(!launcher._held.is_empty());
                    let command = launcher.command();
                    assert_eq!(command.get_program(), launcher.path.as_os_str());
                    let environment: Vec<_> = command.get_envs().collect();
                    assert_eq!(environment.len(), 2);
                    assert!(environment.iter().all(|(name, value)| (name
                        .to_string_lossy()
                        .eq_ignore_ascii_case("SystemRoot")
                        || name.to_string_lossy().eq_ignore_ascii_case("WINDIR"))
                        && *value == Some(launcher.windows_root.as_os_str())));
                }
                return;
            }
            let directory = std::env::temp_dir().join(format!(
                "vox-bench-spoof-synthetic-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).unwrap();
            std::fs::create_dir(directory.join("System32")).unwrap();
            std::fs::copy(
                std::env::current_exe().unwrap(),
                directory.join("System32/wsl.exe"),
            )
            .unwrap();
            // Isolate the forged environment from concurrently running tests.
            // The child only checks identity; it never starts WSL or claims a grant.
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(ROLE, "1")
                .env("SystemRoot", &directory)
                .env("WINDIR", &directory)
                .env("WSLENV", "SYNTHETIC_LAUNCHER_OVERRIDE/p")
                .env(
                    "SYNTHETIC_LAUNCHER_OVERRIDE",
                    directory.join("System32/wsl.exe"),
                )
                .output()
                .unwrap();
            std::fs::remove_dir_all(directory).unwrap();
            assert!(
                output.status.success(),
                "synthetic launcher identity regression failed"
            );
        }

        #[test]
        fn native_acl_and_handle_checks_accept_private_inheritance_and_reject_links() {
            // Native Windows test only. Reuse the controller's exact protected
            // DACL fixture policy, with no app, credentials, WSL or live ledger.
            let source = include_str!("../../../tools/app-bench/windows_effects.py");
            let script = source
                .split("ACL_SCRIPT = r'''")
                .nth(1)
                .unwrap()
                .split("'''")
                .next()
                .unwrap();
            let directory = std::env::temp_dir().join(format!(
                "vox-bench-claim-synthetic-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let system = os_directory(GetSystemDirectoryW).unwrap();
            let output = Command::new(system.join("WindowsPowerShell/v1.0/powershell.exe"))
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &format!("$ErrorActionPreference='Stop';{script}"),
                ])
                .env("VOXGOLEM_BENCH_ACL_PATH", &directory)
                .env("VOXGOLEM_BENCH_ACL_CREATE", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "synthetic private ACL fixture failed"
            );
            {
                let parent = open(&directory, true).unwrap();
                private(&parent, true).unwrap();
                let path = directory.join("synthetic-reservation.json");
                std::fs::write(&path, b"synthetic").unwrap();
                let file = open(&path, false).unwrap();
                private(&file, false).unwrap();
                // An inherited file DACL must not impersonate a protected run.
                assert!(private(&file, true).is_err());
                drop(file);
                std::fs::hard_link(&path, directory.join("synthetic-alias.json")).unwrap();
                assert!(private(&open(&path, false).unwrap(), false).is_err());
            }
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }
}

#[cfg(unix)]
fn claim(path: &Path) -> Result<u64> {
    if !path.is_absolute() {
        return Err(blocked());
    }
    // Reject symlinks in every component, including the private reservation directory.
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)
            .map_err(|_| blocked())?
            .file_type()
            .is_symlink()
        {
            return Err(blocked());
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| blocked())?;
    if !metadata.is_file() || metadata.len() > 1024 {
        return Err(blocked());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(blocked());
        }
        let parent = fs::metadata(path.parent().ok_or_else(blocked)?).map_err(|_| blocked())?;
        if parent.permissions().mode() & 0o077 != 0 {
            return Err(blocked());
        }
    }
    let file = fs::File::open(path).map_err(|_| blocked())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|_| blocked())?;
        if opened.ino() != metadata.ino() || opened.dev() != metadata.dev() {
            return Err(blocked());
        }
    }
    let mut bytes = Vec::new();
    file.take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| blocked())?;
    if bytes.len() > 1024 {
        return Err(blocked());
    }
    let reservation: Reservation = serde_json::from_slice(&bytes).map_err(|_| blocked())?;
    if reservation.schema != 1 || !(1..=12).contains(&reservation.allowance) {
        return Err(blocked());
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".claimed");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(Path::new(&name)).map_err(|_| blocked())?;
    file.sync_all().map_err(|_| blocked())?;
    #[cfg(unix)]
    fs::File::open(path.parent().ok_or_else(blocked)?)
        .and_then(|f| f.sync_all())
        .map_err(|_| blocked())?;
    Ok(reservation.allowance)
}

impl State {
    fn prepare(&mut self, allowance: impl FnOnce() -> Result<u64>) -> Result<()> {
        if self.allowance.is_none() {
            self.allowance = Some(allowance());
            self.counts.remaining = self
                .allowance
                .as_ref()
                .and_then(|a| a.as_ref().ok())
                .copied()
                .unwrap_or(0);
        }
        if !matches!(self.allowance, Some(Ok(_))) {
            return Err(blocked());
        }
        Ok(())
    }

    fn spend(&mut self, allowance: impl FnOnce() -> Result<u64>) -> Result<()> {
        self.prepare(allowance)?;
        if self.counts.remaining == 0 {
            return Err(blocked());
        }
        self.counts.remaining -= 1;
        self.counts.spent += 1;
        Ok(())
    }
}

pub(crate) fn before_generation(config: &CustomOpenAiConfig) -> Result<()> {
    let mut state = STATE.lock().map_err(|_| blocked())?;
    state.counts.generation_attempts += 1;
    validate_generation(config)?;
    if mode()? {
        state.spend(|| {
            let path = std::env::var_os("VOXGOLEM_BENCH_PERMITS").ok_or_else(blocked)?;
            claim(Path::new(&path))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn modes_fail_closed_without_cross_mode_overrides() {
        assert!(parse_mode(None, false, false).is_err());
        assert!(parse_mode(Some("invalid"), false, false).is_err());
        assert!(parse_mode(Some("offline"), true, true).is_err());
        assert!(parse_mode(Some("live"), true, true).is_err());
        assert!(!parse_mode(Some("offline"), false, true).unwrap());
        assert!(parse_mode(Some("live"), true, false).unwrap());
    }

    #[test]
    fn generation_requires_luna_official_live_or_loopback_offline() {
        let mut config = CustomOpenAiConfig {
            model: CustomOpenAiModel::LunaNone,
            ..Default::default()
        };
        assert!(validate_generation_mode(&config, true).is_ok());
        assert!(validate_generation_mode(&config, false).is_err());
        config.model = CustomOpenAiModel::SolHigh;
        assert!(validate_generation_mode(&config, true).is_err());
        config.model = CustomOpenAiModel::LunaNone;
        config.endpoint = "https://example.test/v1/responses".into();
        assert!(validate_generation_mode(&config, true).is_err());
        config.endpoint = "http://127.0.0.1:32123/responses".into();
        assert!(validate_generation_mode(&config, true).is_err());
        assert!(validate_generation_mode(&config, false).is_ok());
    }

    #[test]
    fn numeric_loopback_only() {
        for value in ["http://127.0.0.1:32123/mcp", "http://[::1]:32123/mcp"] {
            assert!(loopback(value, true).is_ok());
        }
        for value in [
            "http://localhost:32123/mcp",
            "https://127.0.0.1:32123/mcp",
            "http://192.0.2.1:32123/mcp",
            "http://u:p@127.0.0.1:32123/mcp",
            "http://127.0.0.1:32123/mcp?q=1",
            "http://127.0.0.1:32123/mcp#x",
            "http://127.0.0.1/mcp",
            "http://127.0.0.1:32123/other",
        ] {
            assert!(loopback(value, true).is_err());
        }
    }

    #[test]
    fn process_cap_and_failed_claim_are_sticky() {
        let mut state = State::default();
        state.prepare(|| Ok(12)).unwrap();
        assert_eq!(state.counts.spent, 0);
        assert_eq!(state.counts.remaining, 12);
        for _ in 0..12 {
            assert!(state.spend(|| Ok(12)).is_ok());
        }
        assert!(state.spend(|| panic!("must not reload")).is_err());
        assert_eq!(state.counts.spent, 12);
        assert_eq!(state.counts.remaining, 0);
        let mut state = State::default();
        assert!(state.spend(|| Err(blocked())).is_err());
        assert!(state
            .spend(|| panic!("must not retry failed claim"))
            .is_err());
    }

    #[cfg(unix)]
    fn fixture(contents: &[u8]) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = Path::new("/tmp/opencode").join(format!(
            "voxgolem-benchmark-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("reservation.json");
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_claim_has_one_winner_and_never_reclaims() {
        let path = fixture(br#"{"schema":1,"allowance":12}"#);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| claim(&path))).collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(claim(&path).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reservation_refuses_bad_content_paths_and_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        for content in [
            br#"{"schema":1,"allowance":13}"#.as_slice(),
            br#"{"schema":2,"allowance":1}"#,
            br#"{"schema":1,"allowance":0}"#,
            b"invalid",
            &[b'x'; 1025],
        ] {
            let path = fixture(content);
            assert!(claim(&path).is_err());
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
        let path = fixture(br#"{"schema":1,"allowance":1}"#);
        let link = path.with_extension("link");
        symlink(&path, &link).unwrap();
        assert!(claim(&link).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(claim(&path).is_err());
        assert!(claim(Path::new("relative.json")).is_err());
        assert!(claim(&path.with_extension("missing")).is_err());
        assert!(claim(path.parent().unwrap()).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
