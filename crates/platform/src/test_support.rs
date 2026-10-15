//! Owned synthetic filesystem/socket fixtures; never touches ambient data.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "desktop")]
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn sse_ending_pairs() -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in ["\n", "\r", "\r\n"] {
        for blank in ["\n", "\r", "\r\n"] {
            let mut separator = format!("{line}{blank}");
            // Adjacent CR + LF is one CRLF line ending. Add the additional
            // newline needed for a blank line, and test that ambiguity separately.
            if line == "\r" && blank == "\n" {
                separator.push('\n');
            }
            pairs.push((format!("{line:?}+{blank:?}"), separator));
        }
    }
    pairs
}

pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("exclusive synthetic fixture directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn accept(listener: &std::net::TcpListener) -> std::net::TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "fixture accept deadline (request may have failed before dispatch)"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture accept: {error}"),
        }
    }
}
