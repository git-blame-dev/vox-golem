//! Bounded local diagnostics. Admission never serializes, waits, or touches disk.
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DAY_MS: u64 = 86_400_000;
const MAX_AGE_MS: u64 = 90 * DAY_MS;
const IDLE: Duration = Duration::from_secs(60);
const CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrontendKind {
    FrontendNotice,
    ExecutionFailed,
    AudioFailed,
    MicrophoneStatusChanged,
    ProfileSwitchFailed,
    TtsToggleFailed,
    BenchmarkLifetimeRetire,
    BenchmarkControlsRetire,
    BenchmarkCapabilityRetire,
    BenchmarkAudioError,
    BenchmarkCueError,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontendEvent {
    pub kind: FrontendKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontendCommand {
    event: FrontendEvent,
}

pub fn frontend_event(value: &serde_json::Value) -> Result<FrontendEvent, &'static str> {
    FrontendCommand::deserialize(value)
        .map(|command| command.event)
        .map_err(|_| "Invalid runtime diagnostic category")
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsEvent {
    Enabled,
    Disabled,
    InitializationStarted,
    InitializationSkipped,
    Ready,
    InitializationFailed,
    RuntimeUnavailable,
    SynthesisFailed,
    PlaybackFailed,
    NativeReplyIncomplete,
    NativeReplyAwaitingRetirement,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "stream", content = "event", rename_all = "snake_case")]
pub enum Record {
    Model(crate::telemetry::TelemetryMetadata),
    Frontend(FrontendKind),
    Tts(TtsEvent),
}

#[derive(Clone, Copy)]
pub struct Config {
    pub model_enabled: bool,
    pub runtime_enabled: bool,
    pub max_bytes: u64,
    pub backup_count: usize,
}

#[derive(Default)]
struct Counters {
    admitted: AtomicU64,
    dropped: AtomicU64,
    written: AtomicU64,
    processed: AtomicU64,
    failures: AtomicU64,
    unavailable: AtomicBool,
    stopping: AtomicBool,
    stopped: AtomicBool,
}

/// Atomic observations, not a consistent transactional snapshot or fsync receipt.
#[derive(Debug, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub admitted: u64,
    pub dropped: u64,
    pub written: u64,
    pub failures: u64,
    pub unavailable: bool,
    pub stopping: bool,
    pub stopped: bool,
}

pub struct Diagnostics {
    sender: Option<mpsc::SyncSender<Work>>,
    counters: Arc<Counters>,
    config: Config,
    observation_clock: fn() -> u64,
}

enum Work {
    Record { record: Record, timestamp_ms: u64 },
    Wake,
}

impl Diagnostics {
    pub fn new(directory: Option<PathBuf>, config: Config) -> Arc<Self> {
        let counters = Arc::new(Counters::default());
        let enabled = config.model_enabled || config.runtime_enabled;
        let sender = if enabled {
            let (sender, receiver) = mpsc::sync_channel(CAPACITY);
            let state = Arc::clone(&counters);
            match std::thread::Builder::new()
                .name("local-diagnostics".into())
                .spawn(move || {
                    worker(receiver, state, || {
                        FileWriter::new(
                            directory
                                .ok_or_else(|| io::Error::other("state directory unavailable"))?,
                            config,
                        )
                    });
                }) {
                Ok(_handle) => Some(sender),
                Err(_) => {
                    counters.failures.fetch_add(1, Ordering::Relaxed);
                    counters.unavailable.store(true, Ordering::Release);
                    counters.stopped.store(true, Ordering::Release);
                    None
                }
            }
        } else {
            counters.stopped.store(true, Ordering::Release);
            None
        };
        Arc::new(Self {
            sender,
            counters,
            config,
            observation_clock: now_ms,
        })
    }

    pub fn record(&self, record: Record) {
        let enabled = match &record {
            Record::Model(_) => self.config.model_enabled,
            _ => self.config.runtime_enabled,
        };
        if !enabled {
            return;
        }
        if self.counters.stopping.load(Ordering::Acquire)
            || self.counters.unavailable.load(Ordering::Acquire)
        {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match self.sender.as_ref().map(|sender| {
            sender.try_send(Work::Record {
                record,
                timestamp_ms: (self.observation_clock)(),
            })
        }) {
            Some(Ok(())) => {
                self.counters.admitted.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn status(&self) -> Status {
        let stopped = self.counters.stopped.load(Ordering::Acquire);
        let admitted = self.counters.admitted.load(Ordering::Relaxed);
        let written = self.counters.written.load(Ordering::Relaxed);
        let processed = self.counters.processed.load(Ordering::Relaxed);
        Status {
            enabled: self.config.model_enabled || self.config.runtime_enabled,
            admitted,
            dropped: self
                .counters
                .dropped
                .load(Ordering::Relaxed)
                .saturating_add(if stopped {
                    admitted.saturating_sub(written)
                } else {
                    processed.saturating_sub(written)
                }),
            written,
            failures: self.counters.failures.load(Ordering::Relaxed),
            unavailable: self.counters.unavailable.load(Ordering::Acquire),
            stopping: self.counters.stopping.load(Ordering::Acquire),
            stopped,
        }
    }

    /// Nonblocking close request. Worker drains at most CAPACITY records. A blocked
    /// filesystem operation cannot be cancelled; stopped remains false in that case.
    pub fn request_shutdown(&self) {
        self.counters.stopping.store(true, Ordering::Release);
        if let Some(sender) = &self.sender {
            // A full queue already wakes the worker; an idle one needs this hint.
            let _ = sender.try_send(Work::Wake);
        }
    }
}

impl Drop for Diagnostics {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

trait Writer {
    /// False means an expired/future observation was deliberately not persisted.
    fn write(&mut self, record: &Record, timestamp_ms: u64) -> io::Result<bool>;
    fn maintain(&mut self) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

fn worker<W: Writer>(
    receiver: mpsc::Receiver<Work>,
    state: Arc<Counters>,
    factory: impl FnOnce() -> io::Result<W>,
) {
    let result = (|| {
        let mut writer = factory()?;
        let mut maintenance = Instant::now();
        loop {
            if state.stopping.load(Ordering::Acquire) {
                // Admissions that raced the close are still bounded by the queue.
                for work in receiver.try_iter().take(CAPACITY) {
                    if let Work::Record {
                        record,
                        timestamp_ms,
                    } = work
                    {
                        persist(&mut writer, &state, &record, timestamp_ms)?;
                    }
                }
                return writer.flush();
            }
            match receiver.recv_timeout(IDLE.saturating_sub(maintenance.elapsed())) {
                Ok(Work::Record {
                    record,
                    timestamp_ms,
                }) => {
                    persist(&mut writer, &state, &record, timestamp_ms)?;
                }
                Ok(Work::Wake) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return writer.flush(),
            }
            if maintenance.elapsed() >= IDLE {
                writer.maintain()?;
                maintenance = Instant::now();
            }
        }
    })();
    if result.is_err() {
        state.failures.fetch_add(1, Ordering::Relaxed);
        state.unavailable.store(true, Ordering::Release);
    }
    state.stopped.store(true, Ordering::Release);
}

fn persist(
    writer: &mut impl Writer,
    state: &Counters,
    record: &Record,
    timestamp_ms: u64,
) -> io::Result<()> {
    if writer.write(record, timestamp_ms)? {
        state.written.fetch_add(1, Ordering::Relaxed);
    }
    state.processed.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Serialize)]
struct Row<'a> {
    schema_version: u8,
    timestamp_ms: u64,
    persisted_ms: u64,
    rotation_sequence: u64,
    process_id: u32,
    event_sequence: u64,
    record: &'a Record,
}

struct FileWriter {
    // Keep the anchored directory and stable, never-unlinked exclusive lock alive.
    _directory: File,
    _lock: File,
    path: PathBuf,
    config: Config,
    sequence: u64,
    clock: fn() -> u64,
}

#[derive(Clone, Copy)]
struct Slot {
    day_ms: u64,
    rotation_sequence: u64,
    len: u64,
}

fn safe_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(target_os = "linux")]
    options.custom_flags(0o400000); // O_NOFOLLOW
    options
}

fn reject_unsafe(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) => {
            Err(io::Error::other("unsafe diagnostics path"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl FileWriter {
    fn new(path: PathBuf, config: Config) -> io::Result<Self> {
        Self::new_with_clock(path, config, now_ms)
    }

    fn new_with_clock(path: PathBuf, config: Config, clock: fn() -> u64) -> io::Result<Self> {
        reject_unsafe(&path)?;
        fs::create_dir_all(&path)?;
        reject_unsafe(&path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(target_os = "linux")]
        options.custom_flags(0o200000 | 0o400000); // O_DIRECTORY | O_NOFOLLOW
        #[cfg(windows)]
        options.custom_flags(0x02000000); // FILE_FLAG_BACKUP_SEMANTICS: open directory via std.
        let directory = options.open(&path)?;
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::other("not a directory"));
        }
        #[cfg(unix)]
        directory.set_permissions(fs::Permissions::from_mode(0o700))?;
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&directory)
        ));
        let lock_path = path.join("writer.lock");
        reject_unsafe(&lock_path)?;
        let lock = safe_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        if !lock.metadata()?.is_file() {
            return Err(io::Error::other("invalid lock file"));
        }
        #[cfg(unix)]
        lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        lock.try_lock().map_err(io::Error::other)?;
        lock.set_len(0)?;
        let mut writer = Self {
            _directory: directory,
            _lock: lock,
            path,
            config,
            sequence: 0,
            clock,
        };
        writer.maintain()?;
        Ok(writer)
    }

    fn slots(&self) -> usize {
        self.config.backup_count.min(255) + 1
    }

    // Fixed ring slots: directory enumeration is unnecessary, so foreign files
    // and an unbounded catalog cannot turn maintenance into unbounded work.
    fn slot(&self, index: usize) -> PathBuf {
        self.path.join(format!("events-{index:03}.jsonl"))
    }

    fn inspect(&self, index: usize, now: u64) -> io::Result<Option<Slot>> {
        use std::io::{BufRead, BufReader};
        let path = self.slot(index);
        reject_unsafe(&path)?;
        let file = match safe_options().read(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("invalid diagnostics file"));
        }
        let len = file.metadata()?.len();
        // All observations in a file belong to one UTC day. Expiring by its
        // start bounds age even if concurrent admissions arrive out of order.
        let mut first = Vec::new();
        BufReader::new(file)
            .take(2049)
            .read_until(b'\n', &mut first)?;
        let header = serde_json::from_slice::<serde_json::Value>(&first)
            .ok()
            .filter(|v| v["schema_version"] == 2)
            .and_then(|v| {
                Some(Slot {
                    day_ms: v["timestamp_ms"].as_u64()? / DAY_MS * DAY_MS,
                    rotation_sequence: v["rotation_sequence"].as_u64()?,
                    len,
                })
            });
        if header.is_none_or(|slot| {
            slot.rotation_sequence == 0
                || slot.day_ms > now
                || now.saturating_sub(slot.day_ms) >= MAX_AGE_MS
        }) || len > self.config.max_bytes
        {
            fs::remove_file(path)?;
            return Ok(None);
        }
        Ok(header)
    }
}

impl Writer for FileWriter {
    fn maintain(&mut self) -> io::Result<()> {
        let now = (self.clock)();
        for index in 0..256 {
            if index < self.slots() {
                self.inspect(index, now)?;
            } else {
                let path = self.slot(index);
                reject_unsafe(&path)?;
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    fn write(&mut self, record: &Record, timestamp_ms: u64) -> io::Result<bool> {
        let now = (self.clock)();
        let mut slots = Vec::with_capacity(self.slots());
        for index in 0..self.slots() {
            slots.push(self.inspect(index, now)?);
        }
        let day_ms = timestamp_ms / DAY_MS * DAY_MS;
        if timestamp_ms > now || now.saturating_sub(day_ms) >= MAX_AGE_MS {
            return Ok(false);
        }
        let latest = slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.map(|slot| (index, slot)))
            .max_by_key(|(_, slot)| slot.rotation_sequence);
        self.sequence = self.sequence.saturating_add(1);
        let mut row = Row {
            schema_version: 2,
            timestamp_ms,
            persisted_ms: now,
            rotation_sequence: latest.map_or(1, |(_, slot)| slot.rotation_sequence),
            process_id: std::process::id(),
            event_sequence: self.sequence,
            record,
        };
        let mut line = serde_json::to_vec(&row).map_err(io::Error::other)?;
        line.push(b'\n');
        if line.len() as u64 > self.config.max_bytes {
            return Err(io::Error::other("record exceeds budget"));
        }
        // Only the latest file can be active; never backfill an older slot.
        let active = latest.filter(|(_, slot)| {
            slot.day_ms == day_ms
                && slot.len.saturating_add(line.len() as u64) <= self.config.max_bytes
        });
        let index = active
            .map(|(index, _)| index)
            .or_else(|| slots.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.unwrap().rotation_sequence)
                    .unwrap()
                    .0
            });
        let replace = active.is_none();
        if replace && latest.is_some() {
            row.rotation_sequence = row
                .rotation_sequence
                .checked_add(1)
                .ok_or_else(|| io::Error::other("diagnostics rotation sequence exhausted"))?;
            // Re-encode only on rotation, not on the ordinary append path.
            line = serde_json::to_vec(&row).map_err(io::Error::other)?;
            line.push(b'\n');
            if line.len() as u64 > self.config.max_bytes {
                return Err(io::Error::other("record exceeds budget"));
            }
        }
        let path = self.slot(index);
        reject_unsafe(&path)?;
        let mut file = safe_options()
            .write(true)
            .create(true)
            .append(!replace)
            .truncate(replace)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("invalid diagnostics file"));
        }
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&line)?;
        Ok(true)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    } // Files are unbuffered; no fsync/durability claim.
}

#[cfg(test)]
pub(crate) struct BlockedTestOwner {
    pub owner: Arc<Diagnostics>,
    entered: mpsc::Receiver<()>,
    release: mpsc::SyncSender<()>,
    done: mpsc::Receiver<()>,
}

#[cfg(test)]
impl BlockedTestOwner {
    pub fn new(path: PathBuf, observation_clock: fn() -> u64, writer_clock: fn() -> u64) -> Self {
        struct BlockedWriter {
            writer: FileWriter,
            entered: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
            blocked: bool,
        }
        impl Writer for BlockedWriter {
            fn write(&mut self, record: &Record, timestamp_ms: u64) -> io::Result<bool> {
                if !self.blocked {
                    self.blocked = true;
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                self.writer.write(record, timestamp_ms)
            }
            fn maintain(&mut self) -> io::Result<()> {
                self.writer.maintain()
            }
            fn flush(&mut self) -> io::Result<()> {
                self.writer.flush()
            }
        }
        let config = Config {
            model_enabled: true,
            runtime_enabled: true,
            max_bytes: 2048,
            backup_count: 1,
        };
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let counters = Arc::new(Counters::default());
        let owner = Arc::new(Diagnostics {
            sender: Some(sender),
            counters: Arc::clone(&counters),
            config,
            observation_clock,
        });
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let (done, done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            worker(receiver, counters, || {
                Ok(BlockedWriter {
                    writer: FileWriter::new_with_clock(path, config, writer_clock)?,
                    entered,
                    release: release_rx,
                    blocked: false,
                })
            });
            done.send(()).unwrap();
        });
        Self {
            owner,
            entered: entered_rx,
            release,
            done: done_rx,
        }
    }

    pub fn wait_blocked(&self) {
        self.entered.recv_timeout(Duration::from_secs(3)).unwrap();
    }

    pub fn finish(self) {
        self.owner.request_shutdown();
        self.release.send(()).unwrap();
        self.done.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(self.owner.status().stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn config() -> Config {
        Config {
            model_enabled: true,
            runtime_enabled: true,
            max_bytes: 2048,
            backup_count: 1,
        }
    }

    fn event() -> Record {
        Record::Frontend(FrontendKind::AudioFailed)
    }

    fn model_event(model: crate::telemetry::Model) -> Record {
        use crate::telemetry::*;
        Record::Model(TelemetryMetadata {
            generation: 7,
            input_source: InputSource::Text,
            provider: Provider::Custom,
            model,
            stage: Stage::Generation,
            transport: Transport::Http,
            inference_provider: InferenceProvider::Remote,
            speculative_origin: SpeculativeOrigin::None,
            input_tokens: None,
            output_tokens: None,
            duration_ms: Some(5),
            status: crate::telemetry::Status::Ok,
            error_category: ErrorCategory::None,
        })
    }

    fn numbered_event(generation: u64) -> Record {
        let Record::Model(mut metadata) = model_event(crate::telemetry::Model::Sol) else {
            unreachable!()
        };
        metadata.generation = generation;
        Record::Model(metadata)
    }

    fn retained_generations(path: &Path) -> Vec<u64> {
        let mut generations = Vec::new();
        let mut total_bytes = 0;
        let mut files = 0;
        for entry in fs::read_dir(path).unwrap().map(Result::unwrap) {
            if entry.file_name() == "writer.lock" {
                continue;
            }
            let text = fs::read_to_string(entry.path()).unwrap();
            assert!(text.len() <= 600);
            total_bytes += text.len();
            files += 1;
            generations.extend(text.lines().map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["record"]["event"]
                    ["generation"]
                    .as_u64()
                    .unwrap()
            }));
        }
        assert!(files <= 3 && total_bytes <= 1800);
        generations.sort_unstable();
        generations
    }

    #[test]
    fn equal_clock_rotations_retain_the_newest_range() {
        let root = tempdir().unwrap();
        let mut writer = FileWriter::new_with_clock(
            root.path().to_path_buf(),
            Config {
                max_bytes: 600,
                backup_count: 2,
                ..config()
            },
            || 1000,
        )
        .unwrap();
        for generation in 1..=10 {
            writer.write(&numbered_event(generation), 1000).unwrap();
        }
        assert_eq!(retained_generations(root.path()), [8, 9, 10]);
    }

    #[test]
    fn restart_preserves_rotation_order_when_timestamps_tie() {
        let root = tempdir().unwrap();
        let config = Config {
            max_bytes: 600,
            backup_count: 2,
            ..config()
        };
        let mut writer =
            FileWriter::new_with_clock(root.path().to_path_buf(), config, || 1000).unwrap();
        for generation in 1..=3 {
            writer.write(&numbered_event(generation), 1000).unwrap();
        }
        drop(writer);
        let mut writer =
            FileWriter::new_with_clock(root.path().to_path_buf(), config, || 1000).unwrap();
        for generation in 4..=5 {
            writer.write(&numbered_event(generation), 1000).unwrap();
        }
        assert_eq!(retained_generations(root.path()), [3, 4, 5]);
    }

    #[test]
    fn blocked_writer_preserves_distinct_native_observation_times() {
        static OBSERVATION: AtomicU64 = AtomicU64::new(11);
        let root = tempdir().unwrap();
        let blocked = BlockedTestOwner::new(
            root.path().to_path_buf(),
            || OBSERVATION.fetch_add(11, Ordering::Relaxed),
            || 1000,
        );
        blocked.owner.record(event());
        blocked.wait_blocked();
        blocked.owner.record(event());
        blocked.finish();
        let text = fs::read_to_string(root.path().join("events-000.jsonl")).unwrap();
        let timestamps: Vec<_> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["timestamp_ms"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(timestamps, [11, 22]);
        for row in text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        {
            assert_eq!(row["persisted_ms"], 1000);
        }
    }

    #[test]
    fn stale_and_future_queued_observations_are_counted_as_dropped_not_written() {
        for observation_clock in [|| 11, || MAX_AGE_MS + 12] {
            let root = tempdir().unwrap();
            let blocked =
                BlockedTestOwner::new(root.path().to_path_buf(), observation_clock, || {
                    MAX_AGE_MS + 11
                });
            let owner = Arc::clone(&blocked.owner);
            owner.record(event());
            blocked.wait_blocked();
            blocked.finish();
            let status = owner.status();
            assert_eq!(
                (
                    status.admitted,
                    status.written,
                    status.dropped,
                    status.failures
                ),
                (1, 0, 1, 0)
            );
            assert!(!root.path().join("events-000.jsonl").exists());
        }
    }

    fn await_stopped(owner: &Diagnostics) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !owner.status().stopped {
            assert!(Instant::now() < deadline, "worker did not retire");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn closed_frontend_contract_rejects_freeform_secrets_and_old_schema() {
        for field in ["detail", "message", "prompt", "request_id", "path"] {
            let value = serde_json::json!({ "kind": "frontend_notice", field: "SYNTHETIC_SECRET_/private/prompt" });
            assert!(serde_json::from_value::<FrontendEvent>(value).is_err());
        }
        for kind in [
            "cue",
            "runtime_control",
            "execution",
            "tts",
            "audio",
            "profile",
            "SYNTHETIC_SECRET",
        ] {
            assert!(
                serde_json::from_value::<FrontendEvent>(serde_json::json!({"kind": kind})).is_err()
            );
        }
        for kind in [
            "frontend_notice",
            "execution_failed",
            "audio_failed",
            "microphone_status_changed",
            "profile_switch_failed",
            "tts_toggle_failed",
            "benchmark_lifetime_retire",
            "benchmark_controls_retire",
            "benchmark_capability_retire",
            "benchmark_audio_error",
            "benchmark_cue_error",
        ] {
            let event: FrontendEvent =
                serde_json::from_value(serde_json::json!({"kind":kind})).unwrap();
            assert_eq!(serde_json::to_value(event.kind).unwrap(), kind);
        }
        assert_eq!(
            frontend_event(&serde_json::json!({"event": {"kind": "audio_failed"}}))
                .unwrap()
                .kind,
            FrontendKind::AudioFailed
        );
        for value in [
            serde_json::json!({"event": {"kind": "audio_failed"}, "detail": "SYNTHETIC_SECRET"}),
            serde_json::json!({"event": {"kind": "audio_failed", "detail": "SYNTHETIC_SECRET"}}),
            serde_json::json!({"kind": "audio_failed"}),
            serde_json::json!({"event": {"kind": "audio"}}),
        ] {
            assert_eq!(
                frontend_event(&value).unwrap_err(),
                "Invalid runtime diagnostic category"
            );
        }
    }

    #[test]
    fn disabled_owner_creates_no_thread_or_files() {
        let root = tempdir().unwrap();
        let path = root.path().join("absent");
        let owner = Diagnostics::new(
            Some(path.clone()),
            Config {
                model_enabled: false,
                runtime_enabled: false,
                ..config()
            },
        );
        owner.record(event());
        assert!(owner.sender.is_none());
        assert!(owner.status().stopped);
        assert!(!owner.status().enabled);
        assert_eq!(owner.status().dropped, 0);
        assert!(!path.exists());
    }

    #[test]
    fn real_worker_persists_only_current_typed_rows_and_flushes_on_close() {
        let root = tempdir().unwrap();
        let owner = Diagnostics::new(Some(root.path().join("diagnostics")), config());
        owner.record(event());
        owner.record(Record::Tts(TtsEvent::Ready));
        owner.request_shutdown();
        await_stopped(&owner);
        let status = owner.status();
        assert_eq!(
            (
                status.admitted,
                status.written,
                status.dropped,
                status.failures
            ),
            (2, 2, 0, 0)
        );
        let rows = fs::read_to_string(root.path().join("diagnostics/events-000.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = rows
            .lines()
            .map(|row| serde_json::from_str(row).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["schema_version"], 2);
        assert!(rows[0]["timestamp_ms"].as_u64().unwrap() > 0);
        assert_eq!(rows[0]["record"]["event"], "audio_failed");
        assert_eq!(rows[1]["record"]["event"], "ready");
        assert_eq!(rows[1]["event_sequence"], 2);
    }

    #[test]
    fn stream_flags_and_closed_model_ids_survive_real_worker_serialization() {
        use crate::telemetry::Model;
        let root = tempdir().unwrap();
        let owner = Diagnostics::new(
            Some(root.path().to_path_buf()),
            Config {
                runtime_enabled: false,
                max_bytes: 8192,
                ..config()
            },
        );
        owner.record(event());
        owner.record(Record::Tts(TtsEvent::Ready));
        for model in [
            Model::GemmaFast,
            Model::GemmaQuality,
            Model::Sol,
            Model::Luna6,
            Model::Luna56,
            Model::Parakeet,
            Model::QwenCompletion,
        ] {
            owner.record(model_event(model));
        }
        owner.request_shutdown();
        await_stopped(&owner);
        assert_eq!(
            (
                owner.status().admitted,
                owner.status().written,
                owner.status().dropped
            ),
            (7, 7, 0)
        );
        let text = fs::read_to_string(root.path().join("events-000.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let models: Vec<_> = rows
            .iter()
            .map(|row| row["record"]["event"]["model"].as_str().unwrap())
            .collect();
        assert_eq!(
            models,
            [
                "gemma-fast",
                "gemma-quality",
                "gpt-6.1-sol",
                "gpt-6-luna",
                "gpt-5.6-luna",
                "parakeet-v2-int8",
                "qwen-completion"
            ]
        );
        for row in rows {
            let metadata = row["record"]["event"].as_object().unwrap();
            assert!(!metadata.contains_key("request_id"));
            assert!(!metadata.contains_key("timestamp_ms"));
            assert_eq!(metadata["generation"], 7);
            assert_eq!(metadata["duration_ms"], 5);
        }
    }

    #[test]
    fn actual_write_failure_discards_unwritten_admissions_visibly() {
        let root = tempdir().unwrap();
        let owner = Diagnostics::new(
            Some(root.path().to_path_buf()),
            Config {
                max_bytes: 1,
                ..config()
            },
        );
        owner.record(event());
        await_stopped(&owner);
        let status = owner.status();
        assert_eq!(
            (
                status.admitted,
                status.written,
                status.dropped,
                status.failures
            ),
            (1, 0, 1, 1)
        );
        assert!(status.enabled && status.unavailable);
        assert!(!root.path().join("events-000.jsonl").exists());
    }

    #[test]
    fn queue_full_and_close_never_wait_for_blocked_production_writer() {
        let root = tempdir().unwrap();
        let blocked = BlockedTestOwner::new(root.path().join("diagnostics"), now_ms, now_ms);
        let owner = Arc::clone(&blocked.owner);
        owner.record(event());
        blocked.wait_blocked();
        let started = Instant::now();
        for _ in 0..CAPACITY + 10 {
            owner.record(event());
        }
        owner.request_shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        let status = owner.status();
        assert_eq!(
            (status.admitted, status.dropped, status.written),
            (129, 10, 0)
        );
        assert!(!status.stopped); // No claim that the blocked thread retired.
        blocked.finish();
        assert_eq!(owner.status().written, 129);
    }

    #[test]
    fn failures_are_visible_without_disabling_enabled_streams() {
        let root = tempdir().unwrap();
        let path = root.path().join("not-a-directory");
        fs::write(&path, b"SYNTHETIC_UNCHANGED").unwrap();
        let owner = Diagnostics::new(Some(path.clone()), config());
        owner.record(event());
        await_stopped(&owner);
        owner.record(event());
        let status = owner.status();
        assert!(status.enabled && status.unavailable);
        assert_eq!(status.failures, 1);
        assert_eq!(status.dropped, 2);
        assert_eq!(fs::read(path).unwrap(), b"SYNTHETIC_UNCHANGED");
    }

    #[test]
    fn actual_row_age_not_recent_file_mtime_controls_retention() {
        let root = tempdir().unwrap();
        let mut writer = FileWriter::new(root.path().to_path_buf(), config()).unwrap();
        writer.clock = || DAY_MS;
        writer.write(&event(), DAY_MS).unwrap();
        writer.clock = || DAY_MS + 100;
        writer.write(&event(), DAY_MS + 100).unwrap();
        assert!(writer.slot(0).exists());
        // mtime is current, but both synthetic rows are at least 90 days old.
        writer.clock = || DAY_MS + 100 + MAX_AGE_MS;
        writer.maintain().unwrap();
        assert!(!writer.slot(0).exists());
    }

    #[test]
    fn daily_rollover_and_byte_file_budgets_include_low_volume_and_config_shrink() {
        let root = tempdir().unwrap();
        let mut writer = FileWriter::new(
            root.path().to_path_buf(),
            Config {
                max_bytes: 300,
                backup_count: 3,
                ..config()
            },
        )
        .unwrap();
        writer.clock = || DAY_MS;
        writer.write(&event(), DAY_MS).unwrap();
        writer.clock = || 2 * DAY_MS;
        writer.write(&event(), 2 * DAY_MS).unwrap();
        assert!(writer.slot(0).exists() && writer.slot(1).exists());
        for _ in 0..30 {
            writer.write(&event(), 2 * DAY_MS).unwrap();
        }
        writer.config.backup_count = 1;
        writer.maintain().unwrap();
        let files: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|file| file.file_name() != "writer.lock")
            .collect();
        assert!(files.len() <= 2);
        assert!(
            files
                .iter()
                .map(|file| file.metadata().unwrap().len())
                .sum::<u64>()
                <= 600
        );
        assert!(files
            .iter()
            .all(|file| file.metadata().unwrap().len() <= 300));
    }

    #[test]
    fn stable_sidecar_excludes_second_writer_until_actual_retirement() {
        let root = tempdir().unwrap();
        let first = FileWriter::new(root.path().to_path_buf(), config()).unwrap();
        assert!(FileWriter::new(root.path().to_path_buf(), config()).is_err());
        assert!(root.path().join("writer.lock").exists());
        drop(first);
        assert!(FileWriter::new(root.path().to_path_buf(), config()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_and_unsafe_paths() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let path = root.path().join("diagnostics");
        let mut writer = FileWriter::new(path.clone(), config()).unwrap();
        writer.write(&event(), now_ms()).unwrap();
        for (path, mode) in [
            (path.clone(), 0o700),
            (path.join("events-000.jsonl"), 0o600),
            (path.join("writer.lock"), 0o600),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        drop(writer);
        let linked = root.path().join("linked");
        symlink(&path, &linked).unwrap();
        assert!(FileWriter::new(linked, config()).is_err());
        let target = root.path().join("target");
        fs::write(&target, b"SYNTHETIC_UNCHANGED").unwrap();
        fs::remove_file(path.join("events-000.jsonl")).unwrap();
        symlink(&target, path.join("events-000.jsonl")).unwrap();
        assert!(FileWriter::new(path, config()).is_err());
        assert_eq!(fs::read(target).unwrap(), b"SYNTHETIC_UNCHANGED");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn anchored_directory_survives_path_replacement_and_rejects_linked_sidecar() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let path = root.path().join("diagnostics");
        let moved = root.path().join("moved");
        let target = root.path().join("other");
        fs::create_dir(&target).unwrap();
        let mut writer = FileWriter::new(path.clone(), config()).unwrap();
        fs::rename(&path, &moved).unwrap();
        symlink(&target, &path).unwrap();
        writer.write(&event(), now_ms()).unwrap();
        assert!(moved.join("events-000.jsonl").exists());
        assert!(!target.join("events-000.jsonl").exists());
        let secret = root.path().join("synthetic-secret");
        fs::write(&secret, b"SYNTHETIC_UNCHANGED").unwrap();
        symlink(&secret, target.join("writer.lock")).unwrap();
        assert!(FileWriter::new(target, config()).is_err());
        assert_eq!(fs::read(secret).unwrap(), b"SYNTHETIC_UNCHANGED");
    }
}
