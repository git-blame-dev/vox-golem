//! Test-only configuration roots, independent of the process environment.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

struct Root {
    path: PathBuf,
    // Captured default contexts keep their directory alive after the source thread exits.
    _directory: Option<tempfile::TempDir>,
}

#[derive(Clone)]
pub(super) struct Context(Arc<Root>);

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

pub(super) fn current() -> Context {
    CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.get_or_insert_with(|| {
            let directory = tempfile::tempdir().expect("create synthetic test configuration root");
            Context(Arc::new(Root {
                path: directory.path().to_path_buf(),
                _directory: Some(directory),
            }))
        })
        .clone()
    })
}

pub(super) fn configuration_path() -> PathBuf {
    current().0.path.join("VoxGolem").join("config.toml")
}

pub(super) fn state_directory() -> PathBuf {
    current().0.path.join("VoxGolem")
}

pub(super) struct ScopedRoot {
    previous: Option<Context>,
    // A scope must restore the same thread that installed it.
    _same_thread: PhantomData<Rc<()>>,
}

impl ScopedRoot {
    pub(super) fn new(root: PathBuf) -> Self {
        Self::install(Context(Arc::new(Root {
            path: root,
            _directory: None,
        })))
    }

    fn install(context: Context) -> Self {
        let previous = CURRENT.with(|slot| slot.replace(Some(context)));
        Self {
            previous,
            _same_thread: PhantomData,
        }
    }
}

impl Drop for ScopedRoot {
    fn drop(&mut self) {
        CURRENT.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

impl Context {
    pub(super) fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        let _scope = ScopedRoot::install(self.clone());
        work()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn runtime_blocking_inherits_explicit_storage_root() {
        let directory = tempfile::tempdir().unwrap();
        let _scope = ScopedRoot::new(directory.path().to_path_buf());
        let expected = configuration_path();
        let actual = tokio::time::timeout(
            Duration::from_secs(5),
            crate::spawn_runtime_blocking(|| (configuration_path(), state_directory())),
        )
        .await
        .expect("blocking work completes within the bound")
        .expect("blocking work succeeds");
        assert_eq!(actual.0, expected);
        assert_eq!(actual.1, directory.path().join("VoxGolem"));
    }

    #[test]
    fn nested_scopes_restore_after_unwind() {
        let original = configuration_path();
        let outer = tempfile::tempdir().unwrap();
        let inner = tempfile::tempdir().unwrap();
        {
            let _outer = ScopedRoot::new(outer.path().to_path_buf());
            let expected = outer.path().join("VoxGolem/config.toml");
            assert_eq!(configuration_path(), expected);
            let result = std::panic::catch_unwind(|| {
                let _inner = ScopedRoot::new(inner.path().to_path_buf());
                assert_eq!(
                    configuration_path(),
                    inner.path().join("VoxGolem/config.toml")
                );
                panic!("synthetic unwind");
            });
            assert!(result.is_err());
            assert_eq!(configuration_path(), expected);
        }
        assert_eq!(configuration_path(), original);
    }

    #[test]
    fn threads_have_independent_synthetic_defaults() {
        let parent = configuration_path();
        let explicit = tempfile::tempdir().unwrap();
        let _scope = ScopedRoot::new(explicit.path().to_path_buf());
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let first = configuration_path();
            assert_eq!(configuration_path(), first);
            assert!(first.parent().unwrap().parent().unwrap().is_dir());
            send.send(first).unwrap();
        });
        let child = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
        assert_ne!(child, parent);
        assert_ne!(child, configuration_path());
        assert_eq!(
            configuration_path(),
            explicit.path().join("VoxGolem/config.toml")
        );
    }

    #[test]
    fn captured_context_is_inherited_and_restores_worker_default() {
        let (send, receive) = std::sync::mpsc::channel();
        let origin = std::thread::spawn(move || send.send(current()).unwrap());
        let captured = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        origin.join().unwrap();
        assert!(captured.0.path.is_dir());
        let expected = captured.0.path.join("VoxGolem/config.toml");
        let (done, completed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let previous = configuration_path();
            captured.run(|| {
                assert_eq!(configuration_path(), expected);
                let result = std::panic::catch_unwind(|| {
                    captured.run(|| panic!("synthetic context unwind"));
                });
                assert!(result.is_err());
                assert_eq!(configuration_path(), expected);
            });
            assert_eq!(configuration_path(), previous);
            done.send(()).unwrap();
        });
        completed.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn captured_explicit_root_preserves_fixture_io() {
        let directory = tempfile::tempdir().unwrap();
        let _scope = ScopedRoot::new(directory.path().to_path_buf());
        let path = configuration_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"synthetic configuration bytes").unwrap();
        let context = current();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            context.run(|| {
                assert_eq!(configuration_path(), path);
                send.send(std::fs::read(configuration_path()).unwrap())
                    .unwrap();
            });
        });
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"synthetic configuration bytes".as_slice()
        );
        worker.join().unwrap();
    }
}
