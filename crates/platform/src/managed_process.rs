//! Reserved process ownership. Drop only publishes into a preallocated mailbox.
//!
//! One supervisor polls all four production ownership lanes. A lane counts the
//! live child AND its cleanup, not just published runtimes. Linux additionally
//! owns one private anchor per lane: never reap it before the final group signal.
//! Thus production retains at most eight direct native children (four leaders
//! plus four anchors), excluding server descendants and WSL guest processes.
//! Exclusive child-wait ownership is required (no external waitpid(-1) reaper).
//! Unavailable workers retain their mailboxes and reject further launches.
//! Process abort/OOM/OS termination cannot provide an in-process wait guarantee.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(any(windows, test))]
pub(crate) const CREATE_SUSPENDED_FLAG: u32 = 0x0000_0004;
#[cfg(any(windows, test))]
const MAX_TOOLHELP_THREAD_RECORDS: usize = 262_144;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessOwnership {
    Owned,
    External,
}

/// The production consumer slots, not a claimed maximum of the old public API.
#[derive(Clone, Copy)]
pub(crate) enum ProcessRole {
    Completion,
    OpenCode,
    LlamaCpp,
    WslCommand,
}

#[derive(Default)]
pub(crate) struct LaunchGate<'a> {
    creation_flags: u32,
    deadline: Option<Instant>,
    cancelled: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
}

impl<'a> LaunchGate<'a> {
    pub(crate) const fn new() -> Self {
        Self {
            creation_flags: 0,
            deadline: None,
            cancelled: None,
        }
    }
    pub(crate) const fn with_creation_flags(mut self, flags: u32) -> Self {
        self.creation_flags = flags;
        self
    }
    pub(crate) const fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }
    pub(crate) fn with_cancellation(
        mut self,
        cancelled: &'a (dyn Fn() -> bool + Send + Sync),
    ) -> Self {
        self.cancelled = Some(cancelled);
        self
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled.is_some_and(|cancelled| cancelled())
    }
    fn deadline_expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

#[derive(Debug)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) enum LaunchError {
    Spawn(io::Error),
    Supervision(String),
    ChildExited(Option<i32>),
    Cancelled,
    TimedOut,
}

impl LaunchError {
    pub(crate) fn into_io_error(self) -> io::Error {
        match self {
            Self::Spawn(error) => error,
            Self::Supervision(details) => io::Error::other(details),
            Self::ChildExited(Some(code)) => io::Error::other(format!(
                "managed process exited before launch completed with code {code}"
            )),
            Self::ChildExited(None) => {
                io::Error::other("managed process exited before launch completed")
            }
            Self::Cancelled => io::Error::new(
                io::ErrorKind::Interrupted,
                "managed process launch cancelled",
            ),
            Self::TimedOut => {
                io::Error::new(io::ErrorKind::TimedOut, "managed process launch timed out")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Pending,
    Retired,
    WaitFailed,
    Unavailable,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The bundle stays in its mailbox while native calls run. Unwinding never
    // moves it out; recovering this known state does not declare it retired.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Mailbox {
    bundle: OnceLock<Mutex<CleanupBundle>>,
    outcome: watch::Sender<Outcome>,
    changed: Condvar,
    wait_lock: Mutex<()>,
    #[cfg(test)]
    fail_wait: AtomicBool,
    #[cfg(test)]
    panic_poll: AtomicBool,
}

impl Mailbox {
    fn new() -> Self {
        Self {
            bundle: OnceLock::new(),
            outcome: watch::channel(Outcome::Pending).0,
            changed: Condvar::new(),
            wait_lock: Mutex::new(()),
            #[cfg(test)]
            fail_wait: AtomicBool::new(false),
            #[cfg(test)]
            panic_poll: AtomicBool::new(false),
        }
    }
    fn outcome(&self) -> Outcome {
        *self.outcome.borrow()
    }
    fn publish_outcome(&self, outcome: Outcome) {
        let _guard = lock(&self.wait_lock);
        self.outcome.send_replace(outcome);
        self.changed.notify_all();
    }
    fn result(&self) -> Option<io::Result<()>> {
        match self.outcome() {
            Outcome::Pending => None,
            Outcome::Retired => Some(Ok(())),
            Outcome::WaitFailed => Some(Err(io::Error::other("managed process wait failed"))),
            Outcome::Unavailable => Some(Err(io::Error::other("process supervisor unavailable"))),
        }
    }
    async fn wait_async(&self, timeout: Duration) -> io::Result<()> {
        let mut receiver = self.outcome.subscribe();
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(result) = self.result() {
                    return result;
                }
                receiver
                    .changed()
                    .await
                    .map_err(|_| io::Error::other("process supervisor unavailable"))?;
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "managed process still retiring"))?
    }
    fn wait(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut guard = lock(&self.wait_lock);
        loop {
            if let Some(result) = self.result() {
                return result;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "managed process still retiring",
                ));
            }
            guard = self
                .changed
                .wait_timeout(guard, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

struct Supervisor {
    slots: Mutex<Vec<Option<Arc<Mailbox>>>>,
    wake: Condvar,
    available: AtomicBool,
    stop: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    #[cfg(test)]
    paused: AtomicBool,
}

impl Supervisor {
    fn start(capacity: usize, fail_start: bool) -> io::Result<Arc<Self>> {
        if fail_start {
            return Err(io::Error::other("process supervisor startup failed"));
        }
        let supervisor = Arc::new(Self {
            slots: Mutex::new(vec![None; capacity]),
            wake: Condvar::new(),
            available: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
            #[cfg(test)]
            paused: AtomicBool::new(false),
        });
        let worker = Arc::clone(&supervisor);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::Builder::new()
            .name("process-supervisor".into())
            .spawn(move || {
                if ready_tx.send(()).is_err() {
                    return;
                }
                let result = std::panic::catch_unwind(|| worker.run());
                if result.is_err() {
                    worker.fail();
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| io::Error::other("process supervisor startup failed"))?;
        *lock(&supervisor.thread) = Some(thread);
        Ok(supervisor)
    }
    fn fail(&self) {
        self.available.store(false, Ordering::Release);
        for mailbox in lock(&self.slots).iter().flatten() {
            if mailbox.outcome() != Outcome::Retired {
                mailbox.publish_outcome(Outcome::Unavailable);
            }
        }
    }
    fn reserve(self: &Arc<Self>, slot: usize) -> io::Result<ProcessReservation> {
        let mut slots = lock(&self.slots);
        if !self.available.load(Ordering::Acquire) {
            return Err(io::Error::other("process supervisor unavailable"));
        }
        let position = slots
            .get_mut(slot)
            .ok_or_else(|| io::Error::other("invalid process lane"))?;
        if position
            .as_ref()
            .is_some_and(|mailbox| mailbox.outcome() != Outcome::Retired)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "managed process lane is occupied",
            ));
        }
        let mailbox = Arc::new(Mailbox::new());
        *position = Some(Arc::clone(&mailbox));
        Ok(ProcessReservation {
            supervisor: Arc::clone(self),
            mailbox,
            published: false,
        })
    }
    fn run(self: &Arc<Self>) {
        let capacity = lock(&self.slots).len();
        while !self.stop.load(Ordering::Acquire) {
            if Arc::strong_count(self) == 1
                && lock(&self.slots)
                    .iter()
                    .flatten()
                    .all(|mailbox| mailbox.outcome() == Outcome::Retired)
            {
                break;
            }
            for index in 0..capacity {
                #[cfg(test)]
                if self.paused.load(Ordering::Acquire) {
                    continue;
                }
                let mailbox = lock(&self.slots)[index].clone();
                let Some(mailbox) = mailbox else { continue };
                if mailbox.outcome() == Outcome::Retired {
                    continue;
                }
                if let Some(bundle) = mailbox.bundle.get() {
                    #[cfg(test)]
                    if mailbox.fail_wait.load(Ordering::Acquire) {
                        mailbox.publish_outcome(Outcome::WaitFailed);
                        continue;
                    }
                    let mut bundle = lock(bundle);
                    #[cfg(any(windows, test))]
                    if bundle.job.as_ref().is_some_and(|job| job.failure.is_some()) {
                        // Job-effect failures stay visibly failed/owned. Do not
                        // repeat OS calls or status publication every poll tick.
                        continue;
                    }
                    #[cfg(test)]
                    assert!(
                        !mailbox.panic_poll.load(Ordering::Acquire),
                        "synthetic worker panic with retained bundle"
                    );
                    match bundle.poll() {
                        Ok(true) => mailbox.publish_outcome(Outcome::Retired),
                        Ok(false) => {}
                        Err(_) => mailbox.publish_outcome(Outcome::WaitFailed),
                    }
                }
            }
            let slots = lock(&self.slots);
            drop(
                self.wake
                    .wait_timeout(slots, POLL_INTERVAL)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }
    async fn wait_for_retirement(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mailboxes = lock(&self.slots).clone();
        for mailbox in mailboxes.into_iter().flatten() {
            mailbox
                .wait_async(deadline.saturating_duration_since(Instant::now()))
                .await?;
        }
        Ok(())
    }
}

static SUPERVISOR: OnceLock<io::Result<Arc<Supervisor>>> = OnceLock::new();
fn supervisor() -> io::Result<&'static Arc<Supervisor>> {
    SUPERVISOR
        .get_or_init(|| Supervisor::start(4, false))
        .as_ref()
        .map_err(|_| io::Error::other("process supervisor startup failed"))
}

/// Do not mistake a server still owned by this process's cleanup lane for an
/// independently managed external HTTP server during fallback/attachment.
pub(crate) fn ensure_role_retired(role: ProcessRole) -> io::Result<()> {
    if let Some(Ok(supervisor)) = SUPERVISOR.get() {
        if lock(&supervisor.slots)[role as usize]
            .as_ref()
            .is_some_and(|mailbox| mailbox.outcome() != Outcome::Retired)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "managed process lane is occupied",
            ));
        }
    }
    Ok(())
}

pub(crate) struct ProcessReservation {
    supervisor: Arc<Supervisor>,
    mailbox: Arc<Mailbox>,
    published: bool,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestProcessPool(Arc<Supervisor>);
#[cfg(test)]
impl std::fmt::Debug for TestProcessPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestProcessPool")
            .finish_non_exhaustive()
    }
}
#[cfg(test)]
impl TestProcessPool {
    pub(crate) fn new() -> Self {
        Self(Supervisor::start(1, false).unwrap())
    }
    pub(crate) fn reserve(&self, _role: ProcessRole) -> io::Result<ProcessReservation> {
        self.0.reserve(0)
    }
}

impl ProcessReservation {
    pub(crate) fn reserve(role: ProcessRole) -> io::Result<Self> {
        supervisor()?.reserve(role as usize)
    }
    fn publish(&mut self, bundle: CleanupBundle) {
        // Exactly one publication per reservation; allocated before real spawn.
        // The persistent slot, not the worker stack, retains this obligation.
        assert!(self.mailbox.bundle.set(Mutex::new(bundle)).is_ok());
        self.published = true;
        if !self.supervisor.available.load(Ordering::Acquire) {
            self.mailbox.outcome.send_replace(Outcome::Unavailable);
        }
        self.supervisor.wake.notify_one();
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn spawn_std(self, command: &mut Command) -> io::Result<ManagedProcess> {
        self.spawn_std_with(command, &LaunchGate::new(), Command::spawn)
            .map_err(LaunchError::into_io_error)
    }
    pub(crate) fn spawn_std_with(
        self,
        command: &mut Command,
        gate: &LaunchGate<'_>,
        spawn: impl FnOnce(&mut Command) -> io::Result<Child>,
    ) -> Result<ManagedProcess, LaunchError> {
        #[cfg(windows)]
        return self.spawn_std_with_driver(command, gate, &RealLaunchDriver, spawn);
        #[cfg(not(windows))]
        return self.spawn_std_unix(command, gate, spawn);
    }
    #[cfg(not(windows))]
    fn spawn_std_unix(
        mut self,
        command: &mut Command,
        gate: &LaunchGate<'_>,
        spawn: impl FnOnce(&mut Command) -> io::Result<Child>,
    ) -> Result<ManagedProcess, LaunchError> {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(LaunchError::Spawn(io::Error::other(
                "process supervisor unavailable",
            )));
        }
        if gate.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            return Err(LaunchError::TimedOut);
        }
        let _ = gate.creation_flags;
        configure_owned(command);
        let child = spawn(command).map_err(LaunchError::Spawn)?;
        let group = match OwnedGroup::new(child.id()) {
            Ok(group) => group,
            Err(error) => {
                self.publish(CleanupBundle::new(
                    ChildKind::Std(child),
                    None,
                    Duration::ZERO,
                ));
                return Err(LaunchError::Spawn(error));
            }
        };
        if gate.is_cancelled() {
            self.publish(CleanupBundle::new(
                ChildKind::Std(child),
                Some(group),
                Duration::ZERO,
            ));
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            self.publish(CleanupBundle::new(
                ChildKind::Std(child),
                Some(group),
                Duration::ZERO,
            ));
            return Err(LaunchError::TimedOut);
        }
        Ok(ManagedProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            #[cfg(any(windows, test))]
            job: None,
        })
    }
    #[cfg(any(windows, test))]
    fn spawn_std_with_driver<D>(
        mut self,
        command: &mut Command,
        gate: &LaunchGate<'_>,
        driver: &D,
        spawn: impl FnOnce(&mut Command) -> io::Result<Child>,
    ) -> Result<ManagedProcess, LaunchError>
    where
        D: LaunchDriver<Job = NativeJob, Thread = NativeThread>,
    {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(LaunchError::Spawn(io::Error::other(
                "process supervisor unavailable",
            )));
        }
        if gate.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            return Err(LaunchError::TimedOut);
        }
        let job = driver.create_job()?;
        configure_owned(command);
        driver.apply_creation_flags_std(command, gate.creation_flags | CREATE_SUSPENDED_FLAG);
        let child = spawn(command).map_err(LaunchError::Spawn)?;
        let pid = child.id();
        let group = match OwnedGroup::new(pid) {
            Ok(group) => group,
            Err(error) => {
                let mut bundle = CleanupBundle::new(ChildKind::Std(child), None, Duration::ZERO);
                bundle.job = Some(JobCleanup::new(job, None));
                self.publish(bundle);
                return Err(LaunchError::Spawn(error));
            }
        };
        let mut child_kind = ChildKind::Std(child);
        let mut thread = None;
        let step = (|| -> Result<(), LaunchError> {
            let ChildKind::Std(ref inner) = child_kind else {
                unreachable!("std child variant preserved");
            };
            let child_handle = driver.std_child_handle(inner)?;
            driver.assign_process(&job, child_handle)?;
            let opened = driver.open_primary_thread(pid)?;
            let verified = driver.verify_and_resume(&mut child_kind, child_handle, &opened, gate);
            thread = Some(opened);
            verified
        })();
        let ChildKind::Std(child) = child_kind else {
            unreachable!("std child variant preserved");
        };
        if let Err(error) = step {
            let mut bundle = CleanupBundle::new(ChildKind::Std(child), Some(group), Duration::ZERO);
            bundle.job = Some(JobCleanup::new(job, thread));
            self.publish(bundle);
            return Err(error);
        }
        Ok(ManagedProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            job: Some((job, thread)),
        })
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn spawn_tokio(
        self,
        command: &mut tokio::process::Command,
    ) -> io::Result<ManagedTokioProcess> {
        self.spawn_tokio_with(command, &LaunchGate::new())
            .map_err(LaunchError::into_io_error)
    }
    pub(crate) fn spawn_tokio_with(
        self,
        command: &mut tokio::process::Command,
        gate: &LaunchGate<'_>,
    ) -> Result<ManagedTokioProcess, LaunchError> {
        #[cfg(windows)]
        return self.spawn_tokio_with_driver(command, gate, &RealLaunchDriver);
        #[cfg(not(windows))]
        return self.spawn_tokio_unix(command, gate);
    }
    #[cfg(not(windows))]
    fn spawn_tokio_unix(
        mut self,
        command: &mut tokio::process::Command,
        gate: &LaunchGate<'_>,
    ) -> Result<ManagedTokioProcess, LaunchError> {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(LaunchError::Spawn(io::Error::other(
                "process supervisor unavailable",
            )));
        }
        if gate.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            return Err(LaunchError::TimedOut);
        }
        let _ = gate.creation_flags;
        configure_owned_tokio(command);
        let child = command.spawn().map_err(LaunchError::Spawn)?;
        let group = match child
            .id()
            .ok_or_else(|| io::Error::other("spawned child has no PID"))
            .and_then(OwnedGroup::new)
        {
            Ok(group) => group,
            Err(error) => {
                self.publish(CleanupBundle::new(
                    ChildKind::Tokio(child),
                    None,
                    Duration::ZERO,
                ));
                return Err(LaunchError::Spawn(error));
            }
        };
        if gate.is_cancelled() {
            self.publish(CleanupBundle::new(
                ChildKind::Tokio(child),
                Some(group),
                Duration::ZERO,
            ));
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            self.publish(CleanupBundle::new(
                ChildKind::Tokio(child),
                Some(group),
                Duration::ZERO,
            ));
            return Err(LaunchError::TimedOut);
        }
        Ok(ManagedTokioProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            #[cfg(any(windows, test))]
            job: None,
        })
    }
    #[cfg(any(windows, test))]
    fn spawn_tokio_with_driver<D>(
        mut self,
        command: &mut tokio::process::Command,
        gate: &LaunchGate<'_>,
        driver: &D,
    ) -> Result<ManagedTokioProcess, LaunchError>
    where
        D: LaunchDriver<Job = NativeJob, Thread = NativeThread>,
    {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(LaunchError::Spawn(io::Error::other(
                "process supervisor unavailable",
            )));
        }
        if gate.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            return Err(LaunchError::TimedOut);
        }
        let job = driver.create_job()?;
        configure_owned_tokio(command);
        driver.apply_creation_flags_tokio(command, gate.creation_flags | CREATE_SUSPENDED_FLAG);
        let child = command.spawn().map_err(LaunchError::Spawn)?;
        let pid = match child.id() {
            Some(pid) => pid,
            None => {
                let mut bundle = CleanupBundle::new(ChildKind::Tokio(child), None, Duration::ZERO);
                bundle.job = Some(JobCleanup::new(job, None));
                self.publish(bundle);
                return Err(LaunchError::Supervision("spawned child has no PID".into()));
            }
        };
        let group = match OwnedGroup::new(pid) {
            Ok(group) => group,
            Err(error) => {
                let mut bundle = CleanupBundle::new(ChildKind::Tokio(child), None, Duration::ZERO);
                bundle.job = Some(JobCleanup::new(job, None));
                self.publish(bundle);
                return Err(LaunchError::Spawn(error));
            }
        };
        let mut child_kind = ChildKind::Tokio(child);
        let mut thread = None;
        let step = (|| -> Result<(), LaunchError> {
            let ChildKind::Tokio(ref inner) = child_kind else {
                unreachable!("tokio child variant preserved");
            };
            let child_handle = driver.tokio_child_handle(inner)?;
            driver.assign_process(&job, child_handle)?;
            let opened = driver.open_primary_thread(pid)?;
            let verified = driver.verify_and_resume(&mut child_kind, child_handle, &opened, gate);
            thread = Some(opened);
            verified
        })();
        let ChildKind::Tokio(child) = child_kind else {
            unreachable!("tokio child variant preserved");
        };
        if let Err(error) = step {
            let mut bundle =
                CleanupBundle::new(ChildKind::Tokio(child), Some(group), Duration::ZERO);
            bundle.job = Some(JobCleanup::new(job, thread));
            self.publish(bundle);
            return Err(error);
        }
        Ok(ManagedTokioProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            job: Some((job, thread)),
        })
    }
}

impl Drop for ProcessReservation {
    fn drop(&mut self) {
        if !self.published {
            self.mailbox.outcome.send_replace(Outcome::Retired);
        }
    }
}

/// Includes active reservations, failed adoption and dropped/abandoned waiters.
pub async fn wait_for_process_retirement(timeout: Duration) -> io::Result<()> {
    // Failed initialization returned no reservation, so no child was launched.
    // That capability failure must not invent an outstanding cleanup obligation.
    let Some(Ok(supervisor)) = SUPERVISOR.get() else {
        return Ok(());
    };
    supervisor.wait_for_retirement(timeout).await
}

// Windows' Tokio Child is larger than std Child. There are only four reserved
// payloads; retain them inline rather than add a per-child box/ownership move.
#[cfg_attr(windows, allow(clippy::large_enum_variant))]
enum ChildKind {
    Std(Child),
    Tokio(tokio::process::Child),
}
impl ChildKind {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Std(child) => child.try_wait(),
            Self::Tokio(child) => child.try_wait(),
        }
    }
    fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        match self {
            Self::Std(child) => child.kill(),
            Self::Tokio(child) => child.start_kill(),
        }
    }
    fn close_stdin(&mut self) {
        match self {
            Self::Std(child) => {
                child.stdin.take();
            }
            Self::Tokio(child) => {
                child.stdin.take();
            }
        }
    }
}

#[cfg(any(windows, test))]
trait JobEffects {
    fn terminate(&self) -> io::Result<()>;
    fn is_empty(&self) -> io::Result<bool>;
}

#[cfg(any(windows, test))]
trait ThreadEffects {
    fn close(self) -> io::Result<()>;
}

/// Keep the effect and its termination/drain state under the same process owner.
#[cfg(any(windows, test))]
struct JobCleanup<J, T> {
    job: J,
    thread: Option<T>,
    termination_requested: bool,
    failure: Option<&'static str>,
}

#[cfg(any(windows, test))]
impl<J: JobEffects, T: ThreadEffects> JobCleanup<J, T> {
    fn new(job: J, thread: Option<T>) -> Self {
        Self {
            job,
            thread,
            termination_requested: false,
            failure: None,
        }
    }
    fn request_termination(&mut self) -> io::Result<()> {
        if let Some(failure) = self.failure {
            return Err(io::Error::other(failure));
        }
        if !self.termination_requested {
            if self.job.terminate().is_err() {
                self.failure = Some("managed job termination failed");
                return Err(io::Error::other("managed job termination failed"));
            }
            self.termination_requested = true;
        }
        Ok(())
    }
    fn drained(&mut self) -> io::Result<bool> {
        if let Some(failure) = self.failure {
            return Err(io::Error::other(failure));
        }
        match self.job.is_empty() {
            Ok(empty) => Ok(empty),
            Err(_) => {
                // Native-effect failures are latched, not retried at the child
                // polling cadence. The slot/handle remain owned and unavailable.
                self.failure = Some("managed job drain query failed");
                Err(io::Error::other("managed job drain query failed"))
            }
        }
    }
    fn close_thread(&mut self) -> io::Result<()> {
        if let Some(failure) = self.failure {
            return Err(io::Error::other(failure));
        }
        if let Some(thread) = self.thread.take() {
            if thread.close().is_err() {
                self.failure = Some("managed primary thread close failed");
                return Err(io::Error::other("managed primary thread close failed"));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
type NativeJob = win32job::Job;
#[cfg(windows)]
type NativeThread = RetainedThread;

#[cfg(all(test, not(windows)))]
type NativeJob = FakeJob;
#[cfg(all(test, not(windows)))]
type NativeThread = FakeThread;

#[cfg(windows)]
struct RetainedThread(isize);

#[cfg(windows)]
impl ThreadEffects for RetainedThread {
    fn close(self) -> io::Result<()> {
        close_raw_handle(self.0)
    }
}

#[cfg(windows)]
impl JobEffects for win32job::Job {
    fn terminate(&self) -> io::Result<()> {
        terminate_job(self)
    }
    fn is_empty(&self) -> io::Result<bool> {
        self.query_process_id_list()
            .map(|processes| processes.is_empty())
            .map_err(|_| io::Error::other("managed job drain query failed"))
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadSnapshotRecord {
    struct_size: u32,
    thread_id: u32,
    owner_process_id: u32,
}

#[cfg(any(windows, test))]
fn select_sole_primary_thread<I>(
    pid: u32,
    minimum_size: u32,
    records: I,
) -> Result<u32, LaunchError>
where
    I: IntoIterator<Item = Result<ThreadSnapshotRecord, String>>,
{
    let mut matched = None;
    for (index, record) in records.into_iter().enumerate() {
        if index >= MAX_TOOLHELP_THREAD_RECORDS {
            return Err(LaunchError::Supervision(
                "Toolhelp thread snapshot exceeded the safety bound".into(),
            ));
        }
        let record = record.map_err(LaunchError::Supervision)?;
        if record.struct_size < minimum_size {
            return Err(LaunchError::Supervision(
                "Toolhelp thread entry was smaller than the required structure prefix".into(),
            ));
        }
        if record.owner_process_id == pid {
            if matched.is_some() {
                return Err(LaunchError::Supervision(
                    "suspended child process had multiple threads before resume".into(),
                ));
            }
            matched = Some(record.thread_id);
        }
    }
    matched.ok_or_else(|| {
        LaunchError::Supervision("suspended child process had no primary thread".into())
    })
}

#[cfg(any(windows, test))]
trait LaunchDriver {
    type Job: JobEffects;
    type Thread: ThreadEffects;

    fn create_job(&self) -> Result<Self::Job, LaunchError>;
    fn apply_creation_flags_std(&self, command: &mut Command, flags: u32);
    fn apply_creation_flags_tokio(&self, command: &mut tokio::process::Command, flags: u32);
    fn std_child_handle(&self, child: &Child) -> Result<isize, LaunchError>;
    fn tokio_child_handle(&self, child: &tokio::process::Child) -> Result<isize, LaunchError>;
    fn assign_process(&self, job: &Self::Job, process_handle: isize) -> Result<(), LaunchError>;
    fn open_primary_thread(&self, pid: u32) -> Result<Self::Thread, LaunchError>;
    fn verify_thread_owner(
        &self,
        process_handle: isize,
        thread: &Self::Thread,
    ) -> Result<(), LaunchError>;
    fn check_process_active(&self, process_handle: isize) -> Result<(), LaunchError>;
    fn check_thread_active(&self, thread: &Self::Thread) -> Result<(), LaunchError>;
    fn resume_thread(&self, thread: &Self::Thread) -> Result<u32, LaunchError>;

    fn verify_and_resume(
        &self,
        child: &mut ChildKind,
        process_handle: isize,
        thread: &Self::Thread,
        gate: &LaunchGate<'_>,
    ) -> Result<(), LaunchError> {
        self.verify_thread_owner(process_handle, thread)?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| LaunchError::Supervision(error.to_string()))?
        {
            return Err(LaunchError::ChildExited(status.code()));
        }
        self.check_process_active(process_handle)?;
        self.check_thread_active(thread)?;
        if gate.is_cancelled() {
            return Err(LaunchError::Cancelled);
        }
        if gate.deadline_expired() {
            return Err(LaunchError::TimedOut);
        }
        let previous_suspend_count = self.resume_thread(thread)?;
        if previous_suspend_count != 1 {
            return Err(LaunchError::Supervision(format!(
                "unexpected primary thread suspend count {previous_suspend_count}"
            )));
        }
        Ok(())
    }
}

#[cfg(windows)]
struct RealLaunchDriver;

#[cfg(windows)]
impl LaunchDriver for RealLaunchDriver {
    type Job = win32job::Job;
    type Thread = RetainedThread;

    fn create_job(&self) -> Result<Self::Job, LaunchError> {
        let mut limits = win32job::ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        win32job::Job::create_with_limit_info(&limits)
            .map_err(|error| LaunchError::Supervision(error.to_string()))
    }
    fn apply_creation_flags_std(&self, command: &mut Command, flags: u32) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(flags);
    }
    fn apply_creation_flags_tokio(&self, command: &mut tokio::process::Command, flags: u32) {
        command.creation_flags(flags);
    }
    fn std_child_handle(&self, child: &Child) -> Result<isize, LaunchError> {
        use std::os::windows::io::AsRawHandle;
        let raw = child.as_raw_handle() as isize;
        if raw == 0 {
            Err(LaunchError::Supervision(
                "spawned child process has no Windows handle".into(),
            ))
        } else {
            Ok(raw)
        }
    }
    fn tokio_child_handle(&self, child: &tokio::process::Child) -> Result<isize, LaunchError> {
        let raw = child.raw_handle().ok_or_else(|| {
            LaunchError::Supervision("spawned child process has no Windows handle".into())
        })? as isize;
        if raw == 0 {
            Err(LaunchError::Supervision(
                "spawned child process has no Windows handle".into(),
            ))
        } else {
            Ok(raw)
        }
    }
    fn assign_process(&self, job: &Self::Job, process_handle: isize) -> Result<(), LaunchError> {
        job.assign_process(process_handle)
            .map_err(|error| LaunchError::Supervision(error.to_string()))
    }
    fn open_primary_thread(&self, pid: u32) -> Result<Self::Thread, LaunchError> {
        let tid = discover_sole_primary_thread_id(pid)?;
        open_thread_handle(tid)
    }
    fn verify_thread_owner(
        &self,
        process_handle: isize,
        thread: &Self::Thread,
    ) -> Result<(), LaunchError> {
        verify_thread_belongs_to_process(process_handle, thread.0)
    }
    fn check_process_active(&self, process_handle: isize) -> Result<(), LaunchError> {
        check_process_still_active(process_handle)
    }
    fn check_thread_active(&self, thread: &Self::Thread) -> Result<(), LaunchError> {
        check_thread_still_active(thread.0)
    }
    fn resume_thread(&self, thread: &Self::Thread) -> Result<u32, LaunchError> {
        resume_thread_once(thread.0)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn terminate_job(job: &win32job::Job) -> io::Result<()> {
    use windows::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
    // win32job documents handle() as a borrowed, still-owned job handle encoded
    // as isize. HANDLE wraps the same pointer-sized value; no ownership transfer
    // or dereference occurs. The borrowed Job stays alive throughout the call.
    let handle = HANDLE(job.handle() as *mut std::ffi::c_void);
    // SAFETY: the live Job owns this valid job-object handle, with termination
    // access from its constructor. Code 1 categorically marks forced shutdown.
    unsafe { TerminateJobObject(handle, 1) }
        .map_err(|_| io::Error::other("managed job termination failed"))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn close_raw_handle(raw: isize) -> io::Result<()> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    let handle = HANDLE(raw as *mut std::ffi::c_void);
    // SAFETY: `raw` is a uniquely owned kernel handle retained in `RetainedThread`
    // or a local scope guard and closed at most once.
    unsafe { CloseHandle(handle) }
        .map_err(|_| io::Error::other("managed primary thread close failed"))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn discover_sole_primary_thread_id(pid: u32) -> Result<u32, LaunchError> {
    use std::mem::{offset_of, size_of};
    use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };

    struct SnapshotGuard(HANDLE);
    impl Drop for SnapshotGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the valid snapshot handle returned by
            // `CreateToolhelp32Snapshot`.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    // SAFETY: `TH32CS_SNAPTHREAD` ignores the process ID argument and returns
    // a newly owned snapshot handle or an error.
    let snapshot = SnapshotGuard(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
            .map_err(|error| LaunchError::Supervision(error.to_string()))?,
    );
    let required_size = (offset_of!(THREADENTRY32, th32OwnerProcessID) + size_of::<u32>()) as u32;
    let entry_size = size_of::<THREADENTRY32>() as u32;
    let mut entry = THREADENTRY32 {
        dwSize: entry_size,
        ..Default::default()
    };
    let mut records = Vec::new();
    // SAFETY: `snapshot.0` is a valid thread snapshot handle and `entry` is
    // initialized with `dwSize = sizeof(THREADENTRY32)`.
    match unsafe { Thread32First(snapshot.0, &mut entry) } {
        Ok(()) => {
            records.push(Ok(ThreadSnapshotRecord {
                struct_size: entry.dwSize,
                thread_id: entry.th32ThreadID,
                owner_process_id: entry.th32OwnerProcessID,
            }));
            while records.len() <= MAX_TOOLHELP_THREAD_RECORDS {
                entry.dwSize = entry_size;
                // SAFETY: `snapshot.0` remains valid and `entry.dwSize` is reset
                // before each `Thread32Next` call as required by Win32.
                match unsafe { Thread32Next(snapshot.0, &mut entry) } {
                    Ok(()) => records.push(Ok(ThreadSnapshotRecord {
                        struct_size: entry.dwSize,
                        thread_id: entry.th32ThreadID,
                        owner_process_id: entry.th32OwnerProcessID,
                    })),
                    Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
                    Err(error) => {
                        records.push(Err(error.to_string()));
                        break;
                    }
                }
            }
        }
        Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => {}
        Err(error) => records.push(Err(error.to_string())),
    }
    select_sole_primary_thread(pid, required_size, records)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn open_thread_handle(tid: u32) -> Result<RetainedThread, LaunchError> {
    use windows::Win32::System::Threading::{
        OpenThread, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };
    // SAFETY: `OpenThread` opens an owned kernel handle to thread `tid` without
    // handle inheritance.
    let handle = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            false,
            tid,
        )
    }
    .map_err(|error| LaunchError::Supervision(error.to_string()))?;
    Ok(RetainedThread(handle.0 as isize))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn verify_thread_belongs_to_process(
    process_handle: isize,
    thread_handle: isize,
) -> Result<(), LaunchError> {
    use windows::Win32::Foundation::{CompareObjectHandles, HANDLE};
    use windows::Win32::System::Threading::{
        GetProcessId, GetProcessIdOfThread, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    struct ProcessHandleGuard(HANDLE);
    impl Drop for ProcessHandleGuard {
        fn drop(&mut self) {
            let _ = close_raw_handle(self.0 .0 as isize);
        }
    }

    let retained_process = HANDLE(process_handle as *mut std::ffi::c_void);
    let retained_thread = HANDLE(thread_handle as *mut std::ffi::c_void);
    // SAFETY: `retained_process` and `retained_thread` are live owned handles.
    let expected_pid = unsafe { GetProcessId(retained_process) };
    if expected_pid == 0 {
        return Err(LaunchError::Supervision(
            "failed to query retained child process ID".into(),
        ));
    }
    // SAFETY: `retained_thread` was opened with `THREAD_QUERY_LIMITED_INFORMATION`.
    let thread_owner_pid = unsafe { GetProcessIdOfThread(retained_thread) };
    if thread_owner_pid == 0 || thread_owner_pid != expected_pid {
        return Err(LaunchError::Supervision(
            "opened thread does not belong to the retained child process".into(),
        ));
    }
    // SAFETY: `OpenProcess` acquires a fresh handle to `thread_owner_pid` so
    // `CompareObjectHandles` can compare underlying kernel process objects.
    let owner_process = ProcessHandleGuard(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, thread_owner_pid) }
            .map_err(|error| LaunchError::Supervision(error.to_string()))?,
    );
    // SAFETY: both handles are valid process handles.
    let same_process = unsafe { CompareObjectHandles(owner_process.0, retained_process) }.as_bool();
    // SAFETY: `retained_thread` remains valid while `owner_process` pins the PID.
    let confirmed_owner_pid = unsafe { GetProcessIdOfThread(retained_thread) };
    if !same_process || confirmed_owner_pid != thread_owner_pid {
        return Err(LaunchError::Supervision(
            "opened thread owner handle did not match the retained child process".into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn check_process_still_active(process_handle: isize) -> Result<(), LaunchError> {
    use windows::Win32::Foundation::{HANDLE, STILL_ACTIVE};
    use windows::Win32::System::Threading::GetExitCodeProcess;
    let handle = HANDLE(process_handle as *mut std::ffi::c_void);
    let mut exit_code = 0_u32;
    // SAFETY: `handle` is the live retained child process handle.
    unsafe { GetExitCodeProcess(handle, &mut exit_code) }
        .map_err(|error| LaunchError::Supervision(error.to_string()))?;
    if exit_code != STILL_ACTIVE.0 as u32 {
        return Err(LaunchError::ChildExited(i32::try_from(exit_code).ok()));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn check_thread_still_active(thread_handle: isize) -> Result<(), LaunchError> {
    use windows::Win32::Foundation::{HANDLE, STILL_ACTIVE};
    use windows::Win32::System::Threading::GetExitCodeThread;
    let handle = HANDLE(thread_handle as *mut std::ffi::c_void);
    let mut exit_code = 0_u32;
    // SAFETY: `handle` is the live retained thread handle opened with
    // `THREAD_QUERY_LIMITED_INFORMATION`.
    unsafe { GetExitCodeThread(handle, &mut exit_code) }
        .map_err(|error| LaunchError::Supervision(error.to_string()))?;
    if exit_code != STILL_ACTIVE.0 as u32 {
        return Err(LaunchError::ChildExited(i32::try_from(exit_code).ok()));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn resume_thread_once(thread_handle: isize) -> Result<u32, LaunchError> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::ResumeThread;
    let handle = HANDLE(thread_handle as *mut std::ffi::c_void);
    // SAFETY: `handle` is the live retained primary thread handle opened with
    // `THREAD_SUSPEND_RESUME`.
    let previous = unsafe { ResumeThread(handle) };
    if previous == u32::MAX {
        return Err(LaunchError::Supervision(
            io::Error::last_os_error().to_string(),
        ));
    }
    Ok(previous)
}

#[cfg(all(test, not(windows)))]
#[derive(Default)]
struct FakeJobState {
    terminate_calls: usize,
    query_calls: usize,
    drops: usize,
    empty: bool,
    terminate_fails: bool,
    query_fails: bool,
    thread_closes: usize,
    thread_drops: usize,
    thread_close_fails: bool,
}
#[cfg(all(test, not(windows)))]
#[derive(Clone)]
struct FakeJob(Arc<Mutex<FakeJobState>>);
#[cfg(all(test, not(windows)))]
impl JobEffects for FakeJob {
    fn terminate(&self) -> io::Result<()> {
        let mut state = lock(&self.0);
        state.terminate_calls += 1;
        if state.terminate_fails {
            Err(io::Error::other("synthetic termination failure"))
        } else {
            Ok(())
        }
    }
    fn is_empty(&self) -> io::Result<bool> {
        let mut state = lock(&self.0);
        state.query_calls += 1;
        if state.query_fails {
            Err(io::Error::other("synthetic query failure"))
        } else {
            Ok(state.empty)
        }
    }
}
#[cfg(all(test, not(windows)))]
impl Drop for FakeJob {
    fn drop(&mut self) {
        lock(&self.0).drops += 1;
    }
}

#[cfg(all(test, not(windows)))]
struct FakeThread(Arc<Mutex<FakeJobState>>);
#[cfg(all(test, not(windows)))]
impl ThreadEffects for FakeThread {
    fn close(self) -> io::Result<()> {
        let mut state = lock(&self.0);
        state.thread_closes += 1;
        if state.thread_close_fails {
            Err(io::Error::other("synthetic thread close failure"))
        } else {
            Ok(())
        }
    }
}
#[cfg(all(test, not(windows)))]
impl Drop for FakeThread {
    fn drop(&mut self) {
        lock(&self.0).thread_drops += 1;
    }
}

struct CleanupBundle {
    child: ChildKind,
    group: Option<OwnedGroup>,
    grace: Duration,
    deadline: Option<Instant>,
    forced: bool,
    request_term: bool,
    #[cfg(any(windows, test))]
    job: Option<JobCleanup<NativeJob, NativeThread>>,
}
impl CleanupBundle {
    fn new(child: ChildKind, group: Option<OwnedGroup>, grace: Duration) -> Self {
        Self {
            child,
            group,
            grace,
            deadline: None,
            forced: false,
            request_term: false,
            #[cfg(any(windows, test))]
            job: None,
        }
    }
    fn poll(&mut self) -> io::Result<bool> {
        if self.deadline.is_none() {
            self.child.close_stdin();
            if self.request_term {
                if let Some(group) = &self.group {
                    terminate_group(group.pid, false)?;
                }
            }
            self.deadline = Some(Instant::now() + self.grace);
        }
        // WSL stdin grace is not evidence of guest retirement. Even if its
        // leader exits, finish the owned host boundary before acknowledging.
        if !self.forced {
            if Instant::now() < self.deadline.unwrap() && self.child.try_wait()?.is_none() {
                return Ok(false);
            }
            if let Some(group) = &mut self.group {
                group.signal_final()?;
            }
            #[cfg(any(windows, test))]
            if let Some(job) = &mut self.job {
                // A reaped leader can still have live job descendants. Never
                // predicate this request on direct-child liveness.
                job.request_termination()?;
            }
            self.child.kill()?;
            self.forced = true;
        }
        let child_retired = self.child.try_wait()?.is_some();
        let anchor_retired = match &mut self.group {
            Some(group) => group.poll_anchor()?,
            None => true,
        };
        #[cfg(any(windows, test))]
        let job_retired = match &mut self.job {
            Some(job) => job.drained()?,
            None => true,
        };
        #[cfg(not(any(windows, test)))]
        let job_retired = true;
        // Termination acceptance is not drain. Retain the job and primary thread
        // handle until direct wait and a successful empty-job query both confirm
        // retirement.
        if child_retired && anchor_retired && job_retired {
            #[cfg(any(windows, test))]
            {
                if let Some(job) = &mut self.job {
                    job.close_thread()?;
                }
                self.job = None;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[derive(Debug)]
struct OwnedGroup {
    pid: u32,
    final_signal_sent: bool,
    #[cfg(target_os = "linux")]
    external: bool,
    #[cfg(target_os = "linux")]
    anchor: Option<Child>,
}
impl OwnedGroup {
    fn new(pid: u32) -> io::Result<Self> {
        #[cfg(all(unix, not(target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned group anchor is not verified on this platform",
        ));
        #[cfg(target_os = "linux")]
        let anchor = {
            use std::os::unix::process::CommandExt;
            use std::process::Stdio;
            let pid = valid_group(pid)?;
            Command::new("/bin/sh")
                .args(["-c", "trap '' TERM HUP INT; IFS= read -r _"])
                .process_group(pid.as_raw_pid())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?
        };
        #[cfg(any(target_os = "linux", windows))]
        Ok(Self {
            pid,
            final_signal_sent: false,
            #[cfg(target_os = "linux")]
            external: false,
            #[cfg(target_os = "linux")]
            anchor: Some(anchor),
        })
    }
    fn signal_final(&mut self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if self.external {
            if let Some(anchor) = &mut self.anchor {
                anchor.kill()?;
            }
        }
        if !self.final_signal_sent {
            terminate_group(self.pid, true)?;
            self.final_signal_sent = true;
        }
        Ok(())
    }
    fn poll_anchor(&mut self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        if let Some(anchor) = &mut self.anchor {
            anchor.stdin.take();
            if anchor.try_wait()?.is_none() {
                return Ok(false);
            }
            self.anchor = None;
        }
        Ok(true)
    }
    fn external_override(&mut self) {
        // Disable group signalling before killing/reaping only our anchor.
        self.final_signal_sent = true;
        #[cfg(target_os = "linux")]
        {
            self.external = true;
        }
    }
}

#[cfg(unix)]
fn valid_group(pid: u32) -> io::Result<rustix::process::Pid> {
    let raw = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process group"))?;
    if raw <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid process group",
        ));
    }
    rustix::process::Pid::from_raw(raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid process group"))
}
fn terminate_group(pid: u32, force: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::process::{kill_process_group, Signal};
        match kill_process_group(
            valid_group(pid)?,
            if force { Signal::KILL } else { Signal::TERM },
        ) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(windows)]
    {
        let _ = (pid, force);
        Ok(())
    }
}
pub fn configure_owned(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    let _ = command;
}
pub fn configure_owned_tokio(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(windows)]
    let _ = command;
}

pub struct ManagedProcess {
    child: Option<Child>,
    group: Option<OwnedGroup>,
    reservation: ProcessReservation,
    grace: Duration,
    #[cfg(any(windows, test))]
    job: Option<(NativeJob, Option<NativeThread>)>,
}
impl std::fmt::Debug for ManagedProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedProcess")
            .field("retirement", &self.reservation.mailbox.outcome())
            .finish_non_exhaustive()
    }
}
impl ManagedProcess {
    pub(crate) fn spawn_owned_with(
        command: &mut Command,
        role: ProcessRole,
        gate: &LaunchGate<'_>,
    ) -> Result<Self, LaunchError> {
        ProcessReservation::reserve(role)
            .map_err(LaunchError::Spawn)?
            .spawn_std_with(command, gate, Command::spawn)
    }
    pub fn id(&self) -> u32 {
        self.child().id()
    }
    pub fn child(&self) -> &Child {
        self.child.as_ref().expect("active managed process")
    }
    pub(crate) fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.as_mut()?.stdin.take()
    }
    pub(crate) fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.as_mut()?.stdout.take()
    }
    pub(crate) fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.as_mut()?.stderr.take()
    }
    pub(crate) fn set_grace(&mut self, grace: Duration) {
        self.grace = grace;
    }
    fn publish(&mut self, graceful: Option<Duration>) {
        if let Some(child) = self.child.take() {
            #[allow(unused_mut)]
            let mut bundle =
                CleanupBundle::new(ChildKind::Std(child), self.group.take(), self.grace);
            #[cfg(unix)]
            if let Some(grace) = graceful.filter(|_| self.grace.is_zero()) {
                bundle.grace = grace;
                bundle.request_term = true;
            }
            #[cfg(windows)]
            let _ = graceful;
            #[cfg(any(windows, test))]
            {
                bundle.job = self
                    .job
                    .take()
                    .map(|(job, thread)| JobCleanup::new(job, thread));
            }
            self.reservation.publish(bundle);
        }
    }
    pub fn terminate(&mut self, timeout: Duration) -> io::Result<()> {
        self.publish(Some(timeout));
        self.reservation.mailbox.wait(timeout.saturating_mul(2))
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("managed process is retiring"))?
            .try_wait()
    }
}
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.publish(None);
    }
}

pub struct ManagedTokioProcess {
    child: Option<tokio::process::Child>,
    group: Option<OwnedGroup>,
    reservation: ProcessReservation,
    grace: Duration,
    #[cfg(any(windows, test))]
    job: Option<(NativeJob, Option<NativeThread>)>,
}
impl ManagedTokioProcess {
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn spawn_test(command: &mut tokio::process::Command) -> io::Result<Self> {
        Supervisor::start(1, false)?
            .reserve(0)?
            .spawn_tokio(command)
    }
    pub(crate) fn spawn_owned_with(
        command: &mut tokio::process::Command,
        role: ProcessRole,
        gate: &LaunchGate<'_>,
    ) -> Result<Self, LaunchError> {
        ProcessReservation::reserve(role)
            .map_err(LaunchError::Spawn)?
            .spawn_tokio_with(command, gate)
    }
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.as_mut()?.stdin.take()
    }
    pub(crate) fn set_grace(&mut self, grace: Duration) {
        self.grace = grace;
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("managed process is retiring"))?
            .try_wait()
    }
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("managed process is retiring"))?
            .wait()
            .await
    }
    fn publish(&mut self, ownership: ProcessOwnership, graceful: Option<Duration>) {
        if let Some(child) = self.child.take() {
            if ownership == ProcessOwnership::External {
                if let Some(group) = &mut self.group {
                    group.external_override();
                }
            }
            #[allow(unused_mut)]
            let mut bundle =
                CleanupBundle::new(ChildKind::Tokio(child), self.group.take(), self.grace);
            #[cfg(unix)]
            if let Some(grace) =
                graceful.filter(|_| self.grace.is_zero() && ownership == ProcessOwnership::Owned)
            {
                bundle.grace = grace;
                bundle.request_term = true;
            }
            #[cfg(windows)]
            let _ = graceful;
            #[cfg(any(windows, test))]
            {
                bundle.job = self
                    .job
                    .take()
                    .map(|(job, thread)| JobCleanup::new(job, thread));
            }
            self.reservation.publish(bundle);
        }
    }
}
impl std::ops::Deref for ManagedTokioProcess {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("active managed process")
    }
}
impl Drop for ManagedTokioProcess {
    fn drop(&mut self) {
        self.publish(ProcessOwnership::Owned, None);
    }
}
pub async fn terminate_tokio(
    child: &mut ManagedTokioProcess,
    ownership: ProcessOwnership,
    timeout: Duration,
) -> io::Result<()> {
    child.publish(ownership, Some(timeout));
    child
        .reservation
        .mailbox
        .wait_async(timeout.saturating_mul(2))
        .await
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;

    const ROLE: &str = "VOXGOLEM_SYNTHETIC_JOB_FIXTURE_ROLE";
    const EXIT_LEADER: &str = "VOXGOLEM_SYNTHETIC_JOB_FIXTURE_EXIT";
    const HELPER_TEST: &str = "managed_process::windows_tests::job_fixture_process";

    #[test]
    fn job_fixture_process() {
        match std::env::var(ROLE).as_deref() {
            Ok("leader") => {
                // Suspended launch assigns the job BEFORE resuming the primary
                // thread; this stdin read only synchronizes test assertions.
                std::io::stdin().read_exact(&mut [0]).unwrap();
                let _descendant = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", HELPER_TEST, "--nocapture"])
                    .env(ROLE, "descendant")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                println!("synthetic-descendant-ready");
                std::io::stdout().flush().unwrap();
                if std::env::var(EXIT_LEADER).as_deref() == Ok("yes") {
                    std::process::exit(0);
                }
                std::thread::sleep(Duration::from_secs(60));
            }
            Ok("descendant") => std::thread::sleep(Duration::from_secs(60)),
            _ => {}
        }
    }

    struct Scope(Arc<Supervisor>);
    impl Drop for Scope {
        fn drop(&mut self) {
            let mailboxes = lock(&self.0.slots).clone();
            for mailbox in mailboxes.iter().flatten() {
                let _ = mailbox.wait(Duration::from_secs(6));
            }
            self.0.stop.store(true, Ordering::Release);
            self.0.wake.notify_one();
            if let Some(thread) = lock(&self.0.thread).take() {
                thread.join().unwrap();
            }
        }
    }

    #[test]
    fn real_job_descendants_retire_on_explicit_shutdown_and_drop_even_after_leader_wait() {
        for explicit in [false, true] {
            for leader_exits in [false, true] {
                let scope = Scope(Supervisor::start(1, false).unwrap());
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", HELPER_TEST, "--nocapture"])
                    .env(ROLE, "leader")
                    .env(EXIT_LEADER, if leader_exits { "yes" } else { "no" })
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                let mut process = scope
                    .0
                    .reserve(0)
                    .unwrap()
                    .spawn_std_with(
                        &mut command,
                        &LaunchGate::new().with_creation_flags(0x0800_0000),
                        Command::spawn,
                    )
                    .unwrap();
                let stdout = process.child.as_mut().unwrap().stdout.take().unwrap();
                let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
                let reader = std::thread::spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        // The test harness can prefix its progress label on the
                        // same line when --nocapture is selected.
                        if line.unwrap().ends_with("synthetic-descendant-ready") {
                            ready_tx.send(()).unwrap();
                            return;
                        }
                    }
                });
                process.take_stdin().unwrap().write_all(b"x").unwrap();
                ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                reader.join().unwrap();
                if leader_exits {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while process.try_wait().unwrap().is_none() {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(POLL_INTERVAL);
                    }
                }
                assert!(
                    process
                        .job
                        .as_ref()
                        .unwrap()
                        .0
                        .query_process_id_list()
                        .unwrap()
                        .len()
                        >= if leader_exits { 1 } else { 2 },
                    "the job must still own a long-lived descendant before shutdown"
                );
                let mailbox = Arc::clone(&process.reservation.mailbox);
                if explicit {
                    process.terminate(Duration::from_secs(3)).unwrap();
                }
                drop(process);
                mailbox.wait(Duration::from_secs(6)).unwrap();
                assert!(
                    lock(mailbox.bundle.get().unwrap()).job.is_none(),
                    "job released only after direct wait and successful empty query"
                );
                drop(scope.0.reserve(0).unwrap());
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    struct Scope(Arc<Supervisor>);
    impl Scope {
        fn new(capacity: usize) -> Self {
            Self(Supervisor::start(capacity, false).unwrap())
        }
        fn reserve(&self, slot: usize) -> ProcessReservation {
            self.0.reserve(slot).unwrap()
        }
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            let mailboxes = lock(&self.0.slots).clone();
            for mailbox in mailboxes.into_iter().flatten() {
                if mailbox.bundle.get().is_some() {
                    let _ = mailbox.wait(Duration::from_secs(3));
                }
            }
            self.0.stop.store(true, Ordering::Release);
            self.0.wake.notify_one();
            if let Some(thread) = lock(&self.0.thread).take() {
                thread.join().unwrap();
            }
        }
    }
    fn command(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    fn wait_child(child: &mut Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic child retirement deadline"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    struct FakeLaunchDriver {
        state: Arc<Mutex<FakeJobState>>,
        events: Arc<Mutex<Vec<&'static str>>>,
        observed_flags: Arc<Mutex<Option<u32>>>,
        create_job_fails: bool,
        assign_fails: bool,
        open_thread_fails: bool,
        owner_handle_mismatch: bool,
        process_exited_before_resume: Option<i32>,
        thread_exited_before_resume: Option<i32>,
        previous_suspend_count: Result<u32, &'static str>,
        resume_calls: Arc<Mutex<usize>>,
    }

    impl FakeLaunchDriver {
        fn new(state: Arc<Mutex<FakeJobState>>) -> Self {
            Self {
                state,
                events: Arc::new(Mutex::new(Vec::new())),
                observed_flags: Arc::new(Mutex::new(None)),
                create_job_fails: false,
                assign_fails: false,
                open_thread_fails: false,
                owner_handle_mismatch: false,
                process_exited_before_resume: None,
                thread_exited_before_resume: None,
                previous_suspend_count: Ok(1),
                resume_calls: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl LaunchDriver for FakeLaunchDriver {
        type Job = FakeJob;
        type Thread = FakeThread;

        fn create_job(&self) -> Result<Self::Job, LaunchError> {
            lock(&self.events).push("create_job");
            if self.create_job_fails {
                Err(LaunchError::Supervision(
                    "synthetic job creation failure".into(),
                ))
            } else {
                Ok(FakeJob(Arc::clone(&self.state)))
            }
        }
        fn apply_creation_flags_std(&self, _command: &mut Command, flags: u32) {
            lock(&self.events).push("apply_flags");
            *lock(&self.observed_flags) = Some(flags);
        }
        fn apply_creation_flags_tokio(&self, _command: &mut tokio::process::Command, flags: u32) {
            lock(&self.events).push("apply_flags");
            *lock(&self.observed_flags) = Some(flags);
        }
        fn std_child_handle(&self, child: &Child) -> Result<isize, LaunchError> {
            Ok(child.id() as isize)
        }
        fn tokio_child_handle(&self, child: &tokio::process::Child) -> Result<isize, LaunchError> {
            child
                .id()
                .map(|pid| pid as isize)
                .ok_or_else(|| LaunchError::Supervision("missing tokio pid".into()))
        }
        fn assign_process(
            &self,
            _job: &Self::Job,
            _process_handle: isize,
        ) -> Result<(), LaunchError> {
            lock(&self.events).push("assign_process");
            if self.assign_fails {
                Err(LaunchError::Supervision(
                    "synthetic job assign failure".into(),
                ))
            } else {
                Ok(())
            }
        }
        fn open_primary_thread(&self, _pid: u32) -> Result<Self::Thread, LaunchError> {
            lock(&self.events).push("open_primary_thread");
            if self.open_thread_fails {
                Err(LaunchError::Supervision(
                    "synthetic open thread failure".into(),
                ))
            } else {
                Ok(FakeThread(Arc::clone(&self.state)))
            }
        }
        fn verify_thread_owner(
            &self,
            _process_handle: isize,
            _thread: &Self::Thread,
        ) -> Result<(), LaunchError> {
            lock(&self.events).push("verify_thread_owner");
            if self.owner_handle_mismatch {
                Err(LaunchError::Supervision(
                    "opened thread owner handle did not match the retained child process".into(),
                ))
            } else {
                Ok(())
            }
        }
        fn check_process_active(&self, _process_handle: isize) -> Result<(), LaunchError> {
            lock(&self.events).push("check_process_active");
            if let Some(code) = self.process_exited_before_resume {
                Err(LaunchError::ChildExited(Some(code)))
            } else {
                Ok(())
            }
        }
        fn check_thread_active(&self, _thread: &Self::Thread) -> Result<(), LaunchError> {
            lock(&self.events).push("check_thread_active");
            if let Some(code) = self.thread_exited_before_resume {
                Err(LaunchError::ChildExited(Some(code)))
            } else {
                Ok(())
            }
        }
        fn resume_thread(&self, _thread: &Self::Thread) -> Result<u32, LaunchError> {
            lock(&self.events).push("resume_thread");
            *lock(&self.resume_calls) += 1;
            self.previous_suspend_count
                .map_err(|details| LaunchError::Supervision(details.into()))
        }
    }

    #[test]
    fn select_sole_primary_thread_enforces_exact_single_match_size_and_bounds() {
        let min_size = 16;
        assert_eq!(
            select_sole_primary_thread(
                42,
                min_size,
                [
                    Ok(ThreadSnapshotRecord {
                        struct_size: min_size,
                        thread_id: 100,
                        owner_process_id: 7,
                    }),
                    Ok(ThreadSnapshotRecord {
                        struct_size: min_size,
                        thread_id: 205,
                        owner_process_id: 42,
                    }),
                ],
            )
            .unwrap(),
            205
        );
        assert!(matches!(
            select_sole_primary_thread(42, min_size, []),
            Err(LaunchError::Supervision(details))
                if details.contains("no primary thread")
        ));
        assert!(matches!(
            select_sole_primary_thread(
                42,
                min_size,
                [
                    Ok(ThreadSnapshotRecord {
                        struct_size: min_size,
                        thread_id: 205,
                        owner_process_id: 42,
                    }),
                    Ok(ThreadSnapshotRecord {
                        struct_size: min_size,
                        thread_id: 206,
                        owner_process_id: 42,
                    }),
                ],
            ),
            Err(LaunchError::Supervision(details))
                if details.contains("multiple threads")
        ));
        assert!(matches!(
            select_sole_primary_thread(
                42,
                min_size,
                [Ok(ThreadSnapshotRecord {
                    struct_size: min_size - 1,
                    thread_id: 205,
                    owner_process_id: 42,
                })],
            ),
            Err(LaunchError::Supervision(details))
                if details.contains("smaller than the required structure prefix")
        ));
        let unbounded = std::iter::repeat_with(|| {
            Ok(ThreadSnapshotRecord {
                struct_size: min_size,
                thread_id: 1,
                owner_process_id: 99,
            })
        })
        .take(MAX_TOOLHELP_THREAD_RECORDS + 1);
        assert!(matches!(
            select_sole_primary_thread(42, min_size, unbounded),
            Err(LaunchError::Supervision(details))
                if details.contains("exceeded the safety bound")
        ));
    }

    #[tokio::test]
    async fn suspended_launch_orders_job_creation_before_spawn_and_preserves_caller_flags() {
        let scope = Scope::new(1);
        let state = Arc::new(Mutex::new(FakeJobState {
            empty: true,
            ..Default::default()
        }));
        let driver = FakeLaunchDriver::new(Arc::clone(&state));
        let events = Arc::clone(&driver.events);
        let gate = LaunchGate::new().with_creation_flags(0x0800_0000);
        let mut process = scope
            .reserve(0)
            .spawn_std_with_driver(&mut command("exec sleep 30"), &gate, &driver, |cmd| {
                lock(&events).push("spawn");
                cmd.spawn()
            })
            .unwrap();
        assert_eq!(
            *lock(&driver.observed_flags),
            Some(0x0800_0000 | CREATE_SUSPENDED_FLAG)
        );
        assert_eq!(
            lock(&driver.events).as_slice(),
            [
                "create_job",
                "apply_flags",
                "spawn",
                "assign_process",
                "open_primary_thread",
                "verify_thread_owner",
                "check_process_active",
                "check_thread_active",
                "resume_thread",
            ]
        );
        assert_eq!(*lock(&driver.resume_calls), 1);
        assert_eq!(lock(&state).thread_closes, 0);
        process.terminate(Duration::from_secs(3)).unwrap();
        assert_eq!(lock(&state).terminate_calls, 1);
        assert_eq!(lock(&state).thread_closes, 1);
        assert_eq!(lock(&state).drops, 1);

        let tokio_state = Arc::new(Mutex::new(FakeJobState {
            empty: true,
            ..Default::default()
        }));
        let tokio_driver = FakeLaunchDriver::new(Arc::clone(&tokio_state));
        let mut tokio_command = tokio::process::Command::new("/bin/sh");
        tokio_command.args(["-c", "exec sleep 30"]);
        let mut tokio_process = scope
            .reserve(0)
            .spawn_tokio_with_driver(&mut tokio_command, &gate, &tokio_driver)
            .unwrap();
        assert_eq!(
            *lock(&tokio_driver.observed_flags),
            Some(0x0800_0000 | CREATE_SUSPENDED_FLAG)
        );
        assert_eq!(*lock(&tokio_driver.resume_calls), 1);
        terminate_tokio(
            &mut tokio_process,
            ProcessOwnership::Owned,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert_eq!(lock(&tokio_state).terminate_calls, 1);
        assert_eq!(lock(&tokio_state).thread_closes, 1);
        assert_eq!(lock(&tokio_state).drops, 1);
    }

    #[test]
    fn post_spawn_failures_retain_child_job_and_thread_until_wait_and_empty_job() {
        #[derive(Clone, Copy)]
        enum FailureCase {
            AssignFailed,
            OpenThreadFailed,
            OwnerHandleMismatch,
            ProcessExitedBeforeResume,
            ThreadExitedBeforeResume,
            CancelledBeforeResume,
            TimedOutBeforeResume,
            SuspendCountZero,
            SuspendCountTwo,
        }
        for case in [
            FailureCase::AssignFailed,
            FailureCase::OpenThreadFailed,
            FailureCase::OwnerHandleMismatch,
            FailureCase::ProcessExitedBeforeResume,
            FailureCase::ThreadExitedBeforeResume,
            FailureCase::CancelledBeforeResume,
            FailureCase::TimedOutBeforeResume,
            FailureCase::SuspendCountZero,
            FailureCase::SuspendCountTwo,
        ] {
            let scope = Scope::new(1);
            scope.0.paused.store(true, Ordering::Release);
            let reservation = scope.reserve(0);
            let mailbox = Arc::clone(&reservation.mailbox);
            let state = Arc::new(Mutex::new(FakeJobState {
                empty: false,
                ..Default::default()
            }));
            let mut driver = FakeLaunchDriver::new(Arc::clone(&state));
            let cancelled = AtomicBool::new(false);
            let check_cancelled = || cancelled.load(Ordering::Acquire);
            let mut gate = LaunchGate::new().with_cancellation(&check_cancelled);
            match case {
                FailureCase::AssignFailed => driver.assign_fails = true,
                FailureCase::OpenThreadFailed => driver.open_thread_fails = true,
                FailureCase::OwnerHandleMismatch => driver.owner_handle_mismatch = true,
                FailureCase::ProcessExitedBeforeResume => {
                    driver.process_exited_before_resume = Some(9)
                }
                FailureCase::ThreadExitedBeforeResume => {
                    driver.thread_exited_before_resume = Some(11)
                }
                FailureCase::CancelledBeforeResume => {}
                FailureCase::TimedOutBeforeResume => {}
                FailureCase::SuspendCountZero => driver.previous_suspend_count = Ok(0),
                FailureCase::SuspendCountTwo => driver.previous_suspend_count = Ok(2),
            }
            let mut pid = 0;
            let result = reservation.spawn_std_with_driver(
                &mut command("exec sleep 30"),
                &gate,
                &driver,
                |cmd| {
                    let child = cmd.spawn()?;
                    pid = child.id();
                    if matches!(case, FailureCase::CancelledBeforeResume) {
                        cancelled.store(true, Ordering::Release);
                    }
                    Ok(child)
                },
            );
            if matches!(case, FailureCase::TimedOutBeforeResume) {
                // Re-run with an already-expired deadline checked immediately before resume.
                drop(result);
                lock(&state).empty = true;
                scope.0.paused.store(false, Ordering::Release);
                mailbox.wait(Duration::from_secs(3)).unwrap();
                scope.0.paused.store(true, Ordering::Release);
                let reservation = scope.reserve(0);
                let mailbox = Arc::clone(&reservation.mailbox);
                let state = Arc::new(Mutex::new(FakeJobState {
                    empty: false,
                    ..Default::default()
                }));
                let driver = FakeLaunchDriver::new(Arc::clone(&state));
                let started = Instant::now();
                gate = LaunchGate::new().with_deadline(started + Duration::from_millis(25));
                let mut pid = 0;
                let result = reservation.spawn_std_with_driver(
                    &mut command("exec sleep 30"),
                    &gate,
                    &driver,
                    |cmd| {
                        let child = cmd.spawn()?;
                        pid = child.id();
                        std::thread::sleep(Duration::from_millis(35));
                        Ok(child)
                    },
                );
                assert!(matches!(result, Err(LaunchError::TimedOut)));
                assert_eq!(*lock(&driver.resume_calls), 0);
                assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(lock(&state).drops, 0);
                assert_eq!(lock(&state).thread_closes, 0);
                assert_eq!(lock(&state).thread_drops, 0);
                scope.0.paused.store(false, Ordering::Release);
                std::thread::sleep(POLL_INTERVAL * 3);
                assert_ne!(mailbox.outcome(), Outcome::Retired);
                assert_eq!(lock(&state).drops, 0);
                assert_eq!(lock(&state).thread_closes, 0);
                lock(&state).empty = true;
                mailbox.wait(Duration::from_secs(3)).unwrap();
                assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
                assert_eq!(lock(&state).thread_closes, 1);
                assert_eq!(lock(&state).drops, 1);
                continue;
            }
            match case {
                FailureCase::AssignFailed
                | FailureCase::OpenThreadFailed
                | FailureCase::OwnerHandleMismatch
                | FailureCase::SuspendCountZero
                | FailureCase::SuspendCountTwo => {
                    assert!(matches!(result, Err(LaunchError::Supervision(_))));
                }
                FailureCase::ProcessExitedBeforeResume | FailureCase::ThreadExitedBeforeResume => {
                    assert!(matches!(result, Err(LaunchError::ChildExited(_))));
                }
                FailureCase::CancelledBeforeResume => {
                    assert!(matches!(result, Err(LaunchError::Cancelled)));
                }
                FailureCase::TimedOutBeforeResume => unreachable!(),
            }
            let expected_resumes = usize::from(matches!(
                case,
                FailureCase::SuspendCountZero | FailureCase::SuspendCountTwo
            ));
            assert_eq!(
                *lock(&driver.resume_calls),
                expected_resumes,
                "ResumeThread must never be called repeatedly or before validation passes"
            );
            assert!(scope.0.reserve(0).is_err());
            assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
            assert_eq!(
                lock(&state).drops,
                0,
                "failed launch must not drop the job outside CleanupBundle"
            );
            assert_eq!(
                lock(&state).thread_closes,
                0,
                "failed launch must not close the thread handle on Drop"
            );
            assert_eq!(
                lock(&state).thread_drops,
                0,
                "failed launch must retain the opened thread handle in CleanupBundle"
            );
            scope.0.paused.store(false, Ordering::Release);
            std::thread::sleep(POLL_INTERVAL * 3);
            assert_ne!(
                mailbox.outcome(),
                Outcome::Retired,
                "bundle must remain owned while job is non-empty"
            );
            assert_eq!(lock(&state).terminate_calls, 1);
            assert_eq!(lock(&state).drops, 0);
            assert_eq!(lock(&state).thread_closes, 0);
            lock(&state).empty = true;
            mailbox.wait(Duration::from_secs(3)).unwrap();
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
            assert_eq!(lock(&state).drops, 1);
            let expected_thread_closes = usize::from(!matches!(
                case,
                FailureCase::AssignFailed | FailureCase::OpenThreadFailed
            ));
            assert_eq!(lock(&state).thread_closes, expected_thread_closes);
        }
    }

    #[test]
    fn reaped_leader_still_requests_job_termination_and_requires_empty_job() {
        let mut child = command("exit 0").spawn().unwrap();
        wait_child(&mut child);
        let state = Arc::new(Mutex::new(FakeJobState::default()));
        let mut bundle = CleanupBundle::new(ChildKind::Std(child), None, Duration::ZERO);
        bundle.job = Some(JobCleanup::new(
            FakeJob(Arc::clone(&state)),
            Some(FakeThread(Arc::clone(&state))),
        ));
        assert!(!bundle.poll().unwrap());
        assert_eq!(
            lock(&state).terminate_calls,
            1,
            "leader wait must not skip job termination"
        );
        assert_eq!(lock(&state).drops, 0, "termination is not drain");
        assert_eq!(
            lock(&state).thread_closes,
            0,
            "primary thread handle stays retained until drain completes"
        );
        // Re-entering the force stage after a direct-child error must not send
        // another job request whose acceptance was already confirmed.
        bundle.job.as_mut().unwrap().request_termination().unwrap();
        assert!(!bundle.poll().unwrap());
        assert_eq!(
            lock(&state).terminate_calls,
            1,
            "successful request is sent only once"
        );
        lock(&state).empty = true;
        assert!(bundle.poll().unwrap());
        assert_eq!(
            lock(&state).thread_closes,
            1,
            "confirmed drain closes the retained primary thread handle"
        );
        assert_eq!(
            lock(&state).drops,
            1,
            "only confirmed drain releases the job"
        );
    }

    #[test]
    fn job_effect_failures_retain_handles_and_capacity_without_poll_retries() {
        for termination_failure in [true, false] {
            let scope = Scope::new(1);
            scope.0.paused.store(true, Ordering::Release);
            let process = scope
                .reserve(0)
                .spawn_std(&mut command("exec sleep 30"))
                .unwrap();
            let mailbox = Arc::clone(&process.reservation.mailbox);
            drop(process);
            let state = Arc::new(Mutex::new(FakeJobState {
                terminate_fails: termination_failure,
                query_fails: !termination_failure,
                ..Default::default()
            }));
            lock(mailbox.bundle.get().unwrap()).job = Some(JobCleanup::new(
                FakeJob(Arc::clone(&state)),
                Some(FakeThread(Arc::clone(&state))),
            ));
            scope.0.paused.store(false, Ordering::Release);
            let error = mailbox.wait(Duration::from_secs(3)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(mailbox.outcome(), Outcome::WaitFailed);
            assert!(scope.0.reserve(0).is_err());
            for _ in 0..3 {
                assert!(lock(mailbox.bundle.get().unwrap()).poll().is_err());
            }
            {
                let state = lock(&state);
                assert_eq!(state.terminate_calls, 1);
                assert_eq!(state.query_calls, usize::from(!termination_failure));
                assert_eq!(state.drops, 0);
                assert_eq!(state.thread_closes, 0);
            }
            // Explicit fixture recovery only: production does not automatically
            // retry a failed native job effect or discard its ownership.
            scope.0.paused.store(true, Ordering::Release);
            {
                let mut bundle = lock(mailbox.bundle.get().unwrap());
                let mut state = lock(&state);
                state.terminate_fails = false;
                state.query_fails = false;
                state.empty = true;
                bundle.job.as_mut().unwrap().failure = None;
                mailbox.publish_outcome(Outcome::Pending);
            }
            scope.0.paused.store(false, Ordering::Release);
            mailbox.wait(Duration::from_secs(3)).unwrap();
            assert_eq!(lock(&state).thread_closes, 1);
            assert_eq!(lock(&state).drops, 1);
        }
    }

    #[test]
    fn reservation_rejects_before_launch_and_drop_is_nonblocking() {
        let scope = Scope::new(1);
        let mut process = scope
            .reserve(0)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        process.set_grace(Duration::from_millis(150));
        let mailbox = Arc::clone(&process.reservation.mailbox);
        assert!(
            matches!(scope.0.reserve(0), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let mut launched = false;
        let rejected = scope.0.reserve(0).and_then(|reservation| {
            reservation
                .spawn_std_with(&mut command("exit 0"), &LaunchGate::new(), |command| {
                    launched = true;
                    command.spawn()
                })
                .map_err(LaunchError::into_io_error)
        });
        assert!(rejected.is_err());
        assert!(
            !launched,
            "capacity is checked before entering the real spawn path"
        );
        let start = Instant::now();
        drop(process);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(scope.0.reserve(0).is_err());
        mailbox.wait(Duration::from_secs(3)).unwrap();
        drop(scope.reserve(0));
    }

    #[test]
    fn startup_failure_has_no_launch_capacity() {
        let mut launches = 0;
        let result = Supervisor::start(1, true).and_then(|supervisor| {
            supervisor
                .reserve(0)?
                .spawn_std_with(&mut command("exit 0"), &LaunchGate::new(), |command| {
                    launches += 1;
                    command.spawn()
                })
                .map_err(LaunchError::into_io_error)
        });
        assert!(result.is_err());
        assert_eq!(launches, 0);
    }

    #[test]
    fn failed_adoption_retains_direct_child_until_wait() {
        let scope = Scope::new(1);
        scope.0.paused.store(true, Ordering::Release);
        let reservation = scope.reserve(0);
        let mailbox = Arc::clone(&reservation.mailbox);
        let mut pid = 0;
        // Production adoption path, with a deliberately unconfigured child.
        let result = reservation.spawn_std_with(&mut command("exit 0"), &LaunchGate::new(), |_| {
            let child = command("exec sleep 30").spawn()?;
            pid = child.id();
            Ok(child)
        });
        assert!(result.is_err());
        assert!(scope.0.reserve(0).is_err());
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        scope.0.paused.store(false, Ordering::Release);
        mailbox.wait(Duration::from_secs(3)).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn wait_failure_and_unavailable_worker_keep_the_bundle_and_capacity() {
        let scope = Scope::new(1);
        scope.0.paused.store(true, Ordering::Release);
        let process = scope
            .reserve(0)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        let mailbox = Arc::clone(&process.reservation.mailbox);
        mailbox.fail_wait.store(true, Ordering::Release);
        drop(process);
        scope.0.paused.store(false, Ordering::Release);
        assert!(mailbox.wait(Duration::from_secs(1)).is_err());
        assert!(scope.0.reserve(0).is_err());
        assert!(mailbox.bundle.get().is_some());
        mailbox.panic_poll.store(true, Ordering::Release);
        mailbox.fail_wait.store(false, Ordering::Release);
        scope.0.wake.notify_one();
        if let Some(thread) = lock(&scope.0.thread).take() {
            thread.join().unwrap();
        }
        assert_eq!(mailbox.outcome(), Outcome::Unavailable);
        assert!(scope.0.reserve(0).is_err());
        // Explicit fixture cleanup, not a production fallback/restarted worker.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !lock(mailbox.bundle.get().unwrap()).poll().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        mailbox.publish_outcome(Outcome::Retired);
        assert!(
            scope.0.reserve(0).is_err(),
            "failed supervisor never admits new work"
        );
    }

    #[test]
    fn pending_mailbox_does_not_starve_sibling() {
        let scope = Scope::new(2);
        let mut first = scope
            .reserve(0)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        first.set_grace(Duration::from_secs(1));
        let first_mailbox = Arc::clone(&first.reservation.mailbox);
        let second = scope
            .reserve(1)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        let second_mailbox = Arc::clone(&second.reservation.mailbox);
        drop(first);
        drop(second);
        second_mailbox.wait(Duration::from_millis(500)).unwrap();
        assert_ne!(first_mailbox.outcome(), Outcome::Retired);
        first_mailbox.wait(Duration::from_secs(3)).unwrap();
    }

    #[test]
    fn drop_after_worker_failure_still_publishes_into_the_reserved_root() {
        let scope = Scope::new(1);
        let process = scope
            .reserve(0)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        let mailbox = Arc::clone(&process.reservation.mailbox);
        scope.0.stop.store(true, Ordering::Release);
        scope.0.wake.notify_one();
        if let Some(thread) = lock(&scope.0.thread).take() {
            thread.join().unwrap();
        }
        scope.0.fail();
        drop(process);
        assert_eq!(mailbox.outcome(), Outcome::Unavailable);
        assert!(mailbox.bundle.get().is_some());
        assert!(scope.0.reserve(0).is_err());
        // Only the test fixture directly retires this unavailable worker's bundle.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !lock(mailbox.bundle.get().unwrap()).poll().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        mailbox.publish_outcome(Outcome::Retired);
    }

    #[tokio::test]
    async fn timeout_and_abandoned_waiter_do_not_abandon_cleanup() {
        let scope = Scope::new(1);
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let mut process = scope.reserve(0).spawn_tokio(&mut command).unwrap();
        process.set_grace(Duration::from_millis(150));
        let mailbox = Arc::clone(&process.reservation.mailbox);
        assert!(scope.0.wait_for_retirement(Duration::ZERO).await.is_err());
        let error = terminate_tokio(&mut process, ProcessOwnership::Owned, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(process);
        assert!(scope.0.reserve(0).is_err());
        mailbox.wait_async(Duration::from_secs(3)).await.unwrap();
        scope.0.wait_for_retirement(Duration::ZERO).await.unwrap();
        let mut process = scope.reserve(0).spawn_tokio(&mut command).unwrap();
        process.set_grace(Duration::from_millis(150));
        let mailbox = Arc::clone(&process.reservation.mailbox);
        assert!(tokio::time::timeout(
            Duration::ZERO,
            terminate_tokio(
                &mut process,
                ProcessOwnership::Owned,
                Duration::from_secs(3)
            )
        )
        .await
        .is_err());
        drop(process);
        assert!(scope.0.reserve(0).is_err());
        mailbox.wait_async(Duration::from_secs(3)).await.unwrap();
    }

    #[test]
    fn dead_anchor_reserves_group_through_final_signal_after_leader_wait() {
        let scope = Scope::new(1);
        let mut process = scope.reserve(0).spawn_std(&mut command("exit 0")).unwrap();
        while process.try_wait().unwrap().is_none() {
            std::thread::sleep(POLL_INTERVAL);
        }
        let group = process.id();
        let anchor = process.group.as_mut().unwrap().anchor.as_mut().unwrap();
        let anchor_pid = anchor.id();
        anchor.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while running(anchor_pid) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        assert!(
            std::path::Path::new(&format!("/proc/{anchor_pid}")).exists(),
            "dead anchor must remain unreaped"
        );
        let mut probe = command("exec sleep 30")
            .process_group(group as i32)
            .spawn()
            .unwrap();
        let mailbox = Arc::clone(&process.reservation.mailbox);
        drop(process);
        mailbox.wait(Duration::from_secs(3)).unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{anchor_pid}")).exists());
        assert!(!wait_child(&mut probe).success());
    }

    #[tokio::test]
    async fn external_override_never_signals_descendant_group() {
        let scope = Scope::new(1);
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "IFS= read -r _"]).stdin(Stdio::piped());
        let mut process = scope.reserve(0).spawn_tokio(&mut command).unwrap();
        let group = process.id().unwrap();
        let mut sentinel = super::tests::command("exec sleep 30")
            .process_group(group as i32)
            .spawn()
            .unwrap();
        terminate_tokio(
            &mut process,
            ProcessOwnership::External,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert!(sentinel.try_wait().unwrap().is_none());
        sentinel.kill().unwrap();
        wait_child(&mut sentinel);
    }

    fn running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .map(|(_, fields)| !fields.trim_start().starts_with(['Z', 'X']))
            })
            .unwrap_or(false)
    }

    #[test]
    fn rejects_unsafe_numeric_groups_without_signalling() {
        for pid in [0, 1, u32::MAX] {
            assert_eq!(
                terminate_group(pid, true).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn explicit_shutdown_allows_graceful_exit_before_final_group_signal() {
        let fixture = crate::test_support::TestDir::new("supervised-graceful");
        let ready = fixture.path().join("synthetic-ready");
        let retired = fixture.path().join("synthetic-retired");
        let scope = Scope::new(1);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap 'printf retired > \"$1\"; exit 0' TERM; printf ready > \"$2\"; while :; do sleep 1; done", "synthetic-graceful"]).arg(&retired).arg(&ready);
        let mut process = scope.reserve(0).spawn_std(&mut command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        process.terminate(Duration::from_secs(1)).unwrap();
        assert_eq!(std::fs::read_to_string(retired).unwrap(), "retired");
    }

    #[test]
    fn historical_group_members_do_not_accumulate_proc_descriptors() {
        use std::os::fd::AsRawFd;
        let scope = Scope::new(1);
        let mut process = scope
            .reserve(0)
            .spawn_std(&mut command("exec sleep 30"))
            .unwrap();
        let group = process.id();
        let anchor = process.group.as_ref().unwrap().anchor.as_ref().unwrap();
        let pipe = std::fs::read_link(format!(
            "/proc/self/fd/{}",
            anchor.stdin.as_ref().unwrap().as_raw_fd()
        ))
        .unwrap();
        let mut retired = Vec::new();
        for _ in 0..64 {
            let mut member = command("exit 0")
                .process_group(group as i32)
                .spawn()
                .unwrap();
            retired.push(member.id());
            wait_child(&mut member);
        }
        let descriptors = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .collect::<Vec<_>>();
        assert_eq!(descriptors.iter().filter(|path| **path == pipe).count(), 1);
        assert!(!descriptors.iter().any(|path| retired.iter().any(|pid| path
            .to_string_lossy()
            .starts_with(&format!("/proc/{pid}/stat")))));
        process.terminate(Duration::from_secs(3)).unwrap();
    }

    #[test]
    fn successor_handoff_survives_leader_and_known_member_wait() {
        use std::io::Write;
        let fixture = crate::test_support::TestDir::new("supervised-successor");
        let marker = fixture.path().join("synthetic-successor.pid");
        let scope = Scope::new(1);
        let mut leader_command = command("IFS= read -r _");
        leader_command.stdin(Stdio::piped());
        let mut leader = scope.reserve(0).spawn_std(&mut leader_command).unwrap();
        let group = leader.id();
        let mut initial = Command::new("/bin/sh");
        initial
            .process_group(group as i32)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        initial.args(["-c", "exec 3<&0; IFS= read -r _; /bin/sh -c 'trap \"\" TERM HUP INT; printf %s \"$$\" > \"$1\"; IFS= read -r _' synthetic-successor \"$1\" <&3 &", "synthetic-initial"]).arg(&marker);
        let mut initial = initial.spawn().unwrap();
        let mut control = initial.stdin.take().unwrap();
        drop(leader.take_stdin());
        let deadline = Instant::now() + Duration::from_secs(3);
        while leader.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        control.write_all(b"handoff\n").unwrap();
        let successor = loop {
            if let Ok(bytes) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = bytes.parse::<u32>() {
                    break pid;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        };
        wait_child(&mut initial);
        assert!(running(successor));
        let mailbox = Arc::clone(&leader.reservation.mailbox);
        drop(leader);
        mailbox.wait(Duration::from_secs(3)).unwrap();
        while running(successor) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL_INTERVAL);
        }
        drop(control);
    }

    #[tokio::test]
    async fn tokio_leader_wait_does_not_release_stubborn_descendants() {
        for explicit in [false, true] {
            let fixture = crate::test_support::TestDir::new("supervised-tokio-descendant");
            let marker = fixture.path().join("synthetic-descendant.pid");
            let scope = Scope::new(1);
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "sh -c 'trap \"\" TERM HUP INT; exec sleep 30' & printf %s $! > \"$1\"; exit 0",
                    "synthetic-leader",
                ])
                .arg(&marker);
            let mut leader = scope.reserve(0).spawn_tokio(&mut command).unwrap();
            leader.wait().await.unwrap();
            assert!(leader.id().is_none());
            let descendant = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
            assert!(running(descendant));
            let mailbox = Arc::clone(&leader.reservation.mailbox);
            if explicit {
                terminate_tokio(&mut leader, ProcessOwnership::Owned, Duration::from_secs(3))
                    .await
                    .unwrap();
            }
            drop(leader);
            mailbox.wait_async(Duration::from_secs(3)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while running(descendant) {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            })
            .await
            .unwrap();
        }
    }
}
