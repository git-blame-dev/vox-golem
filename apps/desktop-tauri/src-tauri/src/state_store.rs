use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{parse_persisted_state, serialize_persisted_state, PersistedState};

pub(super) struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn load(&self) -> Result<PersistedState, String> {
        let _lock = lock_path(&self.path).map_err(|error| format!("lock state: {error}"))?;
        self.load_unlocked()
    }

    pub(super) fn mutate<T>(
        &self,
        change: impl FnOnce(&mut PersistedState) -> Result<T, String>,
    ) -> Result<T, String> {
        let _lock = lock_path(&self.path).map_err(|error| format!("lock state: {error}"))?;
        let mut state = self.load_unlocked()?;
        let result = change(&mut state)?;
        let bytes = serialize_persisted_state(&state)?;
        replace_unlocked(&self.path, bytes.as_bytes())
            .map_err(|error| format!("write state: {error}"))?;
        Ok(result)
    }

    fn load_unlocked(&self) -> Result<PersistedState, String> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => parse_persisted_state(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PersistedState::default()),
            Err(error) => Err(format!("read state: {error}")),
        }
    }
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

// Lock a stable sidecar inode, not the destination inode replaced by rename.
// Dropping the file releases the OS lock; the sidecar must not be removed.
pub(super) fn lock_path(path: &Path) -> io::Result<File> {
    fs::create_dir_all(parent_directory(path))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn replace_unlocked(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(parent_directory(path))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn atomic_bytes_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _lock = lock_path(path)?;
    replace_unlocked(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn roundtrip_and_unrelated_fields_are_retained() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/state.toml");
        let store = StateStore::new(path.clone());
        assert_eq!(store.load().unwrap(), PersistedState::default());
        let original = PersistedState {
            web_enabled: Some(false),
            tts_enabled: Some(true),
            auto_update_download: Some(true),
            auto_update_install: Some(false),
            ..PersistedState::default()
        };
        let serialized = serialize_persisted_state(&original).unwrap();
        atomic_bytes_replace(&path, serialized.as_bytes()).unwrap();
        assert_eq!(store.load().unwrap(), original);
        let returned = store
            .mutate(|state| {
                state.web_enabled = Some(true);
                Ok(17)
            })
            .unwrap();
        assert_eq!(returned, 17);
        let expected = PersistedState {
            web_enabled: Some(true),
            ..original
        };
        assert_eq!(store.load().unwrap(), expected);
        assert_eq!(
            parse_persisted_state(&fs::read_to_string(path).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn malformed_and_unknown_input_is_not_clobbered() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let store = StateStore::new(path.clone());
        for bytes in [
            b"web_enabled = [".as_slice(),
            b"synthetic_unknown_field = true\n".as_slice(),
            b"web_enabled = 'not-a-boolean'\n".as_slice(),
        ] {
            atomic_bytes_replace(&path, bytes).unwrap();
            assert!(store.load().is_err());
            let result = store.mutate(|_| -> Result<(), String> {
                panic!("invalid state must be rejected before invoking the change")
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn closure_failure_preserves_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let bytes = b"# synthetic formatting must survive failure\nweb_enabled = false\n";
        atomic_bytes_replace(&path, bytes).unwrap();
        let result = StateStore::new(path.clone()).mutate(|state| {
            state.web_enabled = Some(true);
            Err::<(), _>(String::from("synthetic rejected change"))
        });
        assert_eq!(result, Err(String::from("synthetic rejected change")));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn failed_temporary_write_preserves_the_same_destination_bytes() {
        use std::os::unix::fs::PermissionsExt;
        struct RestorePermissions(PathBuf, fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, self.1.clone());
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let before = b"# synthetic previous bytes\nweb_enabled = false\n";
        atomic_bytes_replace(&path, before).unwrap();
        let _restore = RestorePermissions(
            directory.path().to_path_buf(),
            fs::metadata(directory.path()).unwrap().permissions(),
        );
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(atomic_bytes_replace(&path, b"web_enabled = true\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    struct Children(Vec<Child>);

    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "coordination file timed out: {path:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn two_process_transactions_preserve_distinct_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.toml");
        let mut children = Children(Vec::new());
        for key in ["web", "tts"] {
            children.0.push(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "state_store::tests::transaction_child",
                        "--nocapture",
                    ])
                    .env("VOX_SYNTHETIC_STATE_STORE_PATH", &path)
                    .env("VOX_SYNTHETIC_STATE_STORE_KEY", key)
                    .spawn()
                    .unwrap(),
            );
            if key == "web" {
                wait_for_file(&directory.path().join("web-loaded"));
            }
        }
        wait_for_file(&directory.path().join("tts-started"));
        fs::write(directory.path().join("release-web"), b"release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        for child in &mut children.0 {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "transaction child failed: {status}");
                    break;
                }
                assert!(Instant::now() < deadline, "transaction child timed out");
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(
            StateStore::new(path).load().unwrap(),
            PersistedState {
                web_enabled: Some(true),
                tts_enabled: Some(false),
                ..PersistedState::default()
            }
        );
    }

    #[test]
    fn transaction_child() {
        let Some(path) = std::env::var_os("VOX_SYNTHETIC_STATE_STORE_PATH") else {
            return;
        };
        let path = PathBuf::from(path);
        let directory = path.parent().unwrap();
        match std::env::var("VOX_SYNTHETIC_STATE_STORE_KEY")
            .unwrap()
            .as_str()
        {
            "web" => StateStore::new(path.clone())
                .mutate(|state| {
                    fs::write(directory.join("web-loaded"), b"loaded").unwrap();
                    wait_for_file(&directory.join("release-web"));
                    state.web_enabled = Some(true);
                    Ok(())
                })
                .unwrap(),
            "tts" => {
                fs::write(directory.join("tts-started"), b"started").unwrap();
                StateStore::new(path.clone())
                    .mutate(|state| {
                        assert_eq!(state.web_enabled, Some(true));
                        state.tts_enabled = Some(false);
                        Ok(())
                    })
                    .unwrap();
            }
            other => panic!("unknown synthetic transaction key: {other}"),
        }
    }
}
