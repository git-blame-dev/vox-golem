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

#[cfg(not(unix))]
fn claim(_path: &Path) -> Result<u64> {
    // Private-file permissions and directory durability are only verified on Unix.
    Err(blocked())
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
