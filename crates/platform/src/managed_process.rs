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
    pub(crate) fn spawn_std(self, command: &mut Command) -> io::Result<ManagedProcess> {
        self.spawn_std_with(command, Command::spawn)
    }
    pub(crate) fn spawn_std_with(
        mut self,
        command: &mut Command,
        spawn: impl FnOnce(&mut Command) -> io::Result<Child>,
    ) -> io::Result<ManagedProcess> {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(io::Error::other("process supervisor unavailable"));
        }
        configure_owned(command);
        let child = spawn(command)?;
        let group = match OwnedGroup::new(child.id()) {
            Ok(group) => group,
            Err(error) => {
                self.publish(CleanupBundle::new(
                    ChildKind::Std(child),
                    None,
                    Duration::ZERO,
                ));
                return Err(error);
            }
        };
        Ok(ManagedProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            #[cfg(windows)]
            job: None,
        })
    }
    pub(crate) fn spawn_tokio(
        mut self,
        command: &mut tokio::process::Command,
    ) -> io::Result<ManagedTokioProcess> {
        if !self.supervisor.available.load(Ordering::Acquire) {
            return Err(io::Error::other("process supervisor unavailable"));
        }
        configure_owned_tokio(command);
        let child = command.spawn()?;
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
                return Err(error);
            }
        };
        Ok(ManagedTokioProcess {
            child: Some(child),
            group: Some(group),
            reservation: self,
            grace: Duration::ZERO,
            #[cfg(windows)]
            job: None,
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

/// Keep the effect and its termination/drain state under the same process owner.
#[cfg(any(windows, test))]
struct JobCleanup<J> {
    job: J,
    termination_requested: bool,
    failure: Option<&'static str>,
}

#[cfg(any(windows, test))]
impl<J: JobEffects> JobCleanup<J> {
    fn new(job: J) -> Self {
        Self {
            job,
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

#[cfg(all(test, not(windows)))]
#[derive(Default)]
struct FakeJobState {
    terminate_calls: usize,
    query_calls: usize,
    drops: usize,
    empty: bool,
    terminate_fails: bool,
    query_fails: bool,
}
#[cfg(all(test, not(windows)))]
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

struct CleanupBundle {
    child: ChildKind,
    group: Option<OwnedGroup>,
    grace: Duration,
    deadline: Option<Instant>,
    forced: bool,
    request_term: bool,
    #[cfg(windows)]
    job: Option<JobCleanup<win32job::Job>>,
    #[cfg(all(test, not(windows)))]
    job: Option<JobCleanup<FakeJob>>,
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
        // Termination acceptance is not drain. Retain the job until direct wait
        // and a successful empty-job query both confirm retirement.
        if child_retired && anchor_retired && job_retired {
            #[cfg(any(windows, test))]
            {
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
    #[cfg(windows)]
    job: Option<win32job::Job>,
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
    pub(crate) fn spawn_owned(command: &mut Command, role: ProcessRole) -> io::Result<Self> {
        ProcessReservation::reserve(role)?.spawn_std(command)
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
    #[cfg(windows)]
    pub(crate) fn set_job(&mut self, job: win32job::Job) {
        self.job = Some(job);
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
            #[cfg(windows)]
            {
                bundle.job = self.job.take().map(JobCleanup::new);
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
    #[cfg(windows)]
    job: Option<win32job::Job>,
}
impl ManagedTokioProcess {
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn spawn_test(command: &mut tokio::process::Command) -> io::Result<Self> {
        Supervisor::start(1, false)?
            .reserve(0)?
            .spawn_tokio(command)
    }
    pub(crate) fn spawn_owned(
        command: &mut tokio::process::Command,
        role: ProcessRole,
    ) -> io::Result<Self> {
        ProcessReservation::reserve(role)?.spawn_tokio(command)
    }
    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.as_mut()?.stdin.take()
    }
    pub(crate) fn set_grace(&mut self, grace: Duration) {
        self.grace = grace;
    }
    #[cfg(windows)]
    pub(crate) fn set_job(&mut self, job: win32job::Job) {
        self.job = Some(job);
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
            #[cfg(windows)]
            {
                bundle.job = self.job.take().map(JobCleanup::new);
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
    use std::os::windows::io::AsRawHandle;
    use std::process::Stdio;

    const ROLE: &str = "VOXGOLEM_SYNTHETIC_JOB_FIXTURE_ROLE";
    const EXIT_LEADER: &str = "VOXGOLEM_SYNTHETIC_JOB_FIXTURE_EXIT";
    const HELPER_TEST: &str = "managed_process::windows_tests::job_fixture_process";

    #[test]
    fn job_fixture_process() {
        match std::env::var(ROLE).as_deref() {
            Ok("leader") => {
                // The parent assigns the job BEFORE allowing descendant spawn.
                // This fixture handshake does not claim race-free app launch.
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
                let mut process = scope.0.reserve(0).unwrap().spawn_std(&mut command).unwrap();
                let mut limits = win32job::ExtendedLimitInfo::new();
                limits.limit_kill_on_job_close();
                let job = win32job::Job::create_with_limit_info(&limits).unwrap();
                job.assign_process(process.child().as_raw_handle() as isize)
                    .unwrap();
                process.set_job(job);
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

    #[test]
    fn reaped_leader_still_requests_job_termination_and_requires_empty_job() {
        let mut child = command("exit 0").spawn().unwrap();
        wait_child(&mut child);
        let state = Arc::new(Mutex::new(FakeJobState::default()));
        let mut bundle = CleanupBundle::new(ChildKind::Std(child), None, Duration::ZERO);
        bundle.job = Some(JobCleanup::new(FakeJob(Arc::clone(&state))));
        assert!(!bundle.poll().unwrap());
        assert_eq!(
            lock(&state).terminate_calls,
            1,
            "leader wait must not skip job termination"
        );
        assert_eq!(lock(&state).drops, 0, "termination is not drain");
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
            lock(mailbox.bundle.get().unwrap()).job =
                Some(JobCleanup::new(FakeJob(Arc::clone(&state))));
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
            reservation.spawn_std_with(&mut command("exit 0"), |command| {
                launched = true;
                command.spawn()
            })
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
                .spawn_std_with(&mut command("exit 0"), |command| {
                    launches += 1;
                    command.spawn()
                })
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
        let result = reservation.spawn_std_with(&mut command("exit 0"), |_| {
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
