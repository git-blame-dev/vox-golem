//! Small, dependency-free process ownership and shutdown primitives.
//!
//! This module deliberately does not install Windows Job Object ownership.  A
//! caller which needs that policy should continue to use the existing Win32
//! job integration instead.

use std::io;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessOwnership {
    /// The child was placed in a process group owned by this helper.
    Owned,
    /// The child was supplied by another owner; only the child itself may be killed.
    External,
}

#[derive(Debug)]
pub struct ManagedProcess {
    child: Child,
    ownership: ProcessOwnership,
    group: Option<OwnedGroup>,
}

/// Retains the ownership boundary independently of Tokio's disappearing Child.id().
/// Construct immediately after spawning with configure_owned_tokio, before wait
/// or try_wait. Owned groups require exclusive child-wait ownership: no external
/// waitpid(-1) reaper or SIGCHLD auto-reap policy may consume the private anchor.
pub struct ManagedTokioProcess {
    child: tokio::process::Child,
    group: Option<OwnedGroup>,
}

impl ManagedTokioProcess {
    pub fn from_owned_child(mut child: tokio::process::Child) -> io::Result<Self> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("spawned child has no PID"))?;
        let group = match OwnedGroup::new(pid) {
            Ok(group) => group,
            Err(error) => {
                // Fail closed: without the anchor, do not signal a numeric
                // group. Only the retained child handle is retired here.
                let _ = child.start_kill();
                reap_tokio_child(child);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            group: Some(group),
        })
    }

    pub fn attach_external(child: tokio::process::Child) -> Self {
        Self { child, group: None }
    }

    pub fn take_stdin(&mut self) -> Option<tokio::process::ChildStdin> {
        self.child.stdin.take()
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        self.child.start_kill()
    }

    fn signal_group(&mut self, force: bool) -> io::Result<()> {
        if let Some(group) = &mut self.group {
            group.terminate(force)?;
        }
        Ok(())
    }

    fn force_group_once(&mut self) -> io::Result<()> {
        self.signal_group(true)?;
        // Forced shutdown is the final group operation. Do not signal a stale
        // numeric boundary a second time from Drop after all members retire.
        self.group = None;
        Ok(())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

impl std::ops::Deref for ManagedTokioProcess {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl Drop for ManagedTokioProcess {
    fn drop(&mut self) {
        let _ = self.signal_group(true);
        let _ = self.child.start_kill();
    }
}

#[derive(Debug)]
struct OwnedGroup {
    pid: u32,
    final_signal_sent: bool,
    #[cfg(target_os = "linux")]
    anchor: Option<Child>,
}

impl OwnedGroup {
    #[cfg(all(unix, not(target_os = "linux")))]
    fn new(_pid: u32) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "unreaped group-anchor reservation is not verified on this Unix platform",
        ))
    }

    #[cfg(any(target_os = "linux", windows))]
    fn new(pid: u32) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        let anchor = {
            use std::os::unix::process::CommandExt;
            use std::process::Stdio;
            let group = i32::try_from(pid)
                .map_err(|_| io::Error::other("owned process group ID is out of range"))?;
            // spawn completes setpgid/exec before returning. The leader is
            // still unreaped here; never poll either Child before this join.
            Command::new("/bin/sh")
                .args(["-c", "trap '' TERM HUP INT; IFS= read -r _"])
                .process_group(group)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("could not establish owned process group anchor: {error}"),
                    )
                })?
        };
        Ok(Self {
            pid,
            final_signal_sent: false,
            #[cfg(target_os = "linux")]
            anchor: Some(anchor),
        })
    }

    fn terminate(&mut self, force: bool) -> io::Result<()> {
        if !self.final_signal_sent {
            // The private std Child is NEVER waited/try_waited before FINAL
            // KILL. Even an unexpectedly dead anchor remains an unreaped group
            // member: Linux detaches PIDTYPE_PGID in release_task, not exit.
            // Unlike proc FDs, this reservation spans the signal syscall.
            platform::terminate_group(self.pid, force)?;
            if force {
                self.final_signal_sent = true;
            }
        }
        #[cfg(target_os = "linux")]
        if force {
            if let Some(anchor) = &mut self.anchor {
                drop(anchor.stdin.take());
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if anchor.try_wait()?.is_some() {
                        self.anchor = None;
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "owned process group anchor did not retire after final signal",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        Ok(())
    }

    fn abandon(&mut self) -> io::Result<()> {
        // An explicit External override must not signal the former group.
        // Retire only our private anchor and permanently disable group signals.
        self.final_signal_sent = true;
        #[cfg(target_os = "linux")]
        if let Some(anchor) = &mut self.anchor {
            anchor.kill()?;
        }
        self.terminate(true)
    }
}

impl Drop for OwnedGroup {
    fn drop(&mut self) {
        if self.terminate(true).is_err() {
            // This is the LAST group attempt. Permanently disable group signals
            // before releasing its reservation, then retire only our private
            // child. Descendant retirement is not claimed on signal failure.
            let _ = self.abandon();
        }
    }
}

/// Configure a `tokio::process::Command` exactly like `spawn_owned` configures
/// a standard command.  Kept here so all runtimes share the ownership rule.
pub fn configure_owned_tokio(command: &mut tokio::process::Command) {
    platform::configure_owned_tokio(command);
}

pub fn configure_owned(command: &mut Command) {
    platform::configure_owned(command);
}

/// Low-level signalling only while an unreaped member reserves the PGID.
/// Observing a numeric PID or a proc descriptor is not such a reservation.
/// Prefer the managed wrappers when a child may already have been reaped.
pub fn terminate_group(pid: u32, force: bool) -> io::Result<()> {
    platform::terminate_group(pid, force)
}

/// Terminate and reap a tokio child, signalling its owned group on Unix.
pub async fn terminate_tokio(
    child: &mut ManagedTokioProcess,
    ownership: ProcessOwnership,
    timeout: Duration,
) -> io::Result<()> {
    if ownership == ProcessOwnership::External {
        if let Some(mut group) = child.group.take() {
            group.abandon()?;
        }
    }
    if ownership == ProcessOwnership::Owned && child.group.is_some() {
        if child.try_wait()?.is_some() {
            return child.force_group_once();
        }
        child.signal_group(false)?;
        platform::kill_tokio_child(&mut child.child)?;
        let grace = tokio::time::timeout(timeout, child.wait()).await;
        child.force_group_once()?;
        platform::kill_tokio_child(&mut child.child)?;
        if let Ok(status) = grace {
            return status.map(|_| ());
        }
        return tokio::time::timeout(timeout, child.wait())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "managed process did not exit"))?
            .map(|_| ());
    }
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.start_kill()?;
    if let Ok(status) = tokio::time::timeout(timeout, child.wait()).await {
        return status.map(|_| ());
    }
    child.start_kill()?;
    tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "managed process did not exit"))?
        .map(|_| ())
}

pub fn terminate_tokio_on_drop(mut child: ManagedTokioProcess, ownership: ProcessOwnership) {
    if ownership == ProcessOwnership::External {
        if let Some(mut group) = child.group.take() {
            let _ = group.abandon();
        }
    }
    if ownership == ProcessOwnership::Owned {
        let _ = child.try_wait();
        let _ = child.force_group_once();
    }
    let _ = child.start_kill();
    let _ = std::thread::Builder::new()
        .name(String::from("managed-process-reaper"))
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let _ = runtime.block_on(child.wait());
        });
}

fn reap_tokio_child(mut child: tokio::process::Child) {
    let _ = std::thread::Builder::new()
        .name(String::from("failed-owned-process-reaper"))
        .spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                let _ = runtime.block_on(child.wait());
            }
        });
}

fn reap_std_child(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name(String::from("failed-owned-process-reaper"))
        .spawn(move || {
            let _ = child.wait();
        });
}

impl ManagedProcess {
    /// Spawn a child in a new process group on Unix.
    pub fn spawn_owned(command: &mut Command) -> io::Result<Self> {
        platform::configure_owned(command);
        Self::from_owned_child(command.spawn()?)
    }

    /// Adopt a freshly spawned, configured group before any wait/try_wait.
    /// This shares the same anchor and exclusive child-wait contract as Tokio.
    pub(crate) fn from_owned_child(mut child: Child) -> io::Result<Self> {
        let group = match OwnedGroup::new(child.id()) {
            Ok(group) => group,
            Err(error) => {
                let _ = child.kill();
                reap_std_child(child);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            ownership: ProcessOwnership::Owned,
            group: Some(group),
        })
    }

    /// Manage an already-created child without claiming its process group.
    pub fn attach_external(child: Child) -> Self {
        Self {
            child,
            ownership: ProcessOwnership::External,
            group: None,
        }
    }

    pub fn ownership(&self) -> ProcessOwnership {
        self.ownership
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn child(&self) -> &Child {
        &self.child
    }

    pub(crate) fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }
    pub(crate) fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }
    pub(crate) fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    /// Stop the child, escalating from a graceful request to a forced kill.
    /// Owned children use group termination; externally attached children use
    /// direct child termination only.
    pub fn terminate(&mut self, timeout: Duration) -> io::Result<()> {
        if self.ownership == ProcessOwnership::Owned && self.group.is_some() {
            self.group.as_mut().unwrap().terminate(false)?;
            platform::kill_child(&mut self.child)?;
            // Keep the group as the ownership boundary even if its leader
            // exits during the grace period.
            let _ = self.wait_until_exited(timeout)?;
            // The dedicated anchor remains unreaped while the original leader
            // is waited, through this final signal, and across descendant handoff.
            self.group.as_mut().unwrap().terminate(true)?;
            self.group = None;
            platform::kill_child(&mut self.child)?;
            return self.wait_until_exited(timeout)?.map(|_| ()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "managed process did not exit")
            });
        }

        self.child.kill()?;
        if self.wait_until_exited(timeout)?.is_some() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "managed process did not exit",
            ))
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    fn wait_until_exited(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if self.ownership == ProcessOwnership::Owned && self.group.is_some() {
            // Reaping the leader does not release descendants from its group.
            // Signal the ownership boundary even when the leader is gone.
            if self.group.as_mut().unwrap().terminate(true).is_ok() {
                self.group = None;
            }
            if self.try_wait().ok().flatten().is_none() {
                let _ = self.terminate(Duration::from_secs(2));
            }
        } else if self.try_wait().ok().flatten().is_none() {
            let _ = self.terminate(Duration::from_secs(2));
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;

    pub fn configure_owned(command: &mut Command) {
        command.process_group(0);
    }

    pub fn configure_owned_tokio(command: &mut tokio::process::Command) {
        command.process_group(0);
    }

    pub fn terminate_group(pid: u32, force: bool) -> io::Result<()> {
        // `kill` is the safe, dependency-free std-process bridge to Unix
        // signals; a negative PID targets the child's process group.
        let group = format!("-{pid}");
        let signal = if force { "-KILL" } else { "-TERM" };
        let kill_path = if cfg!(target_os = "linux") {
            "/usr/bin/kill"
        } else {
            "/bin/kill"
        };
        let output = Command::new(kill_path)
            .args([signal, "--", &group])
            .env("LC_ALL", "C")
            .output()?;
        // A child can exit between try_wait and this command.  Treat that
        // race as successful cleanup; the subsequent wait is authoritative.
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr)
                .to_ascii_lowercase()
                .contains("no such process")
        {
            Ok(())
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ))
        }
    }

    pub fn kill_child(_child: &mut Child) -> io::Result<()> {
        Ok(())
    }

    pub fn kill_tokio_child(_child: &mut tokio::process::Child) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn configure_owned(_command: &mut Command) {}

    pub fn configure_owned_tokio(_command: &mut tokio::process::Command) {}

    pub fn terminate_group(_pid: u32, _force: bool) -> io::Result<()> {
        // Job ownership remains with the existing Win32 integration.  The
        // lifecycle helper falls back to Child::kill below.
        Ok(())
    }

    pub fn kill_child(child: &mut Child) -> io::Result<()> {
        if child.try_wait()?.is_none() {
            child.kill()?
        }
        Ok(())
    }

    pub fn kill_tokio_child(child: &mut tokio::process::Child) -> io::Result<()> {
        if child.try_wait()?.is_none() {
            child.start_kill()?
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn review_kernel_unreaped_anchor_preserves_group_even_after_anchor_death() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        use std::process::Stdio;
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        configure_owned_tokio(&mut command);
        let mut leader = command.spawn().unwrap();
        let group = leader.id().unwrap();
        async fn zombie(pid: u32) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
                    if stat
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .starts_with('Z')
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("owned zombie acknowledgement without any wait/try_wait");
        }
        zombie(group).await;
        // The live Tokio runtime has observed SIGCHLD but does not reap this
        // retained, unpolled Child. Its group can still be joined before wait.
        let mut anchor = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM HUP INT; IFS= read -r _"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(group as i32)
            .spawn()
            .expect("join before leader reap");
        leader.wait().await.unwrap();
        assert!(leader.id().is_none());
        anchor.kill().unwrap(); // std Child::kill does NOT reap.
        zombie(anchor.id()).await;
        // This would fail with EPERM if the sole zombie group member released
        // the PGID on exit instead of retaining it until its parent reaps it.
        let mut probe = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .process_group(group as i32)
            .spawn()
            .expect("unreaped dead anchor must still reserve the group");
        platform::terminate_group(group, true).unwrap();
        let anchor_status = anchor.wait().unwrap(); // ONLY after final group signal.
        assert_eq!(anchor_status.signal(), Some(9));
        assert_eq!(probe.wait().unwrap().signal(), Some(9));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn anchor_join_failure_is_reported_without_claiming_an_external_group() {
        let mut sentinel = ManagedProcess::attach_external(
            Command::new("/bin/sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .unwrap(),
        );
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]).kill_on_drop(true);
        // Deliberately NOT configure_owned: this child belongs to the external
        // parent group, and no group with its numeric PID may be claimed.
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        let error = match ManagedTokioProcess::from_owned_child(child) {
            Ok(_) => panic!("constructor must reject an unjoinable owned group"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("could not establish owned process group anchor"));
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "unrelated child must remain running"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("failed-constructor child retirement");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn explicit_external_override_retires_only_its_anchor_and_child() {
        struct FixtureGroup(OwnedGroup);
        impl Drop for FixtureGroup {
            fn drop(&mut self) {
                let _ = self.0.terminate(true);
            }
        }
        let fixture = crate::test_support::TestDir::new("external-override");
        let marker = fixture.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("sleep 30 & echo $! > '{}'; wait", marker.display()),
        ]);
        configure_owned_tokio(&mut command);
        let mut process = ManagedTokioProcess::from_owned_child(command.spawn().unwrap()).unwrap();
        let mut cleanup = FixtureGroup(OwnedGroup::new(process.id().unwrap()).unwrap());
        let descendant = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(bytes) = std::fs::read_to_string(&marker) {
                    if let Ok(pid) = bytes.trim().parse::<u32>() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        terminate_tokio(
            &mut process,
            ProcessOwnership::External,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        drop(process);
        assert!(
            linux_process_running(descendant),
            "external override must not trigger a group signal from OwnedGroup Drop"
        );
        cleanup.0.terminate(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while linux_process_running(descendant) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn review_successor_handoff_after_leader_and_known_member_reap() {
        use std::io::Write;
        use std::process::Stdio;
        for on_drop in [false, true] {
            let fixture = crate::test_support::TestDir::new("group-handoff");
            let marker = fixture.path().join("successor.pid");
            let mut command = tokio::process::Command::new("/bin/sh");
            command.args(["-c", "IFS= read -r _"]).stdin(Stdio::piped());
            configure_owned_tokio(&mut command);
            let mut process =
                ManagedTokioProcess::from_owned_child(command.spawn().unwrap()).unwrap();
            let group = process.id().unwrap();
            let mut initial = Command::new("/bin/sh");
            use std::os::unix::process::CommandExt;
            initial
                .process_group(group as i32)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            initial.args(["-c", "exec 3<&0; IFS= read -r _; /bin/sh -c 'trap \"\" TERM HUP INT; printf %s \"$$\" > \"$1\"; IFS= read -r _' synthetic-successor \"$1\" <&3 &", "synthetic-initial"]).arg(&marker);
            let mut initial = ManagedProcess::attach_external(initial.spawn().unwrap());
            let mut control = initial.take_stdin().unwrap();
            drop(process.take_stdin());
            tokio::time::timeout(Duration::from_secs(3), process.wait())
                .await
                .unwrap()
                .unwrap();
            assert!(process.id().is_none());
            control.write_all(b"handoff\n").unwrap();
            let successor = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(bytes) = std::fs::read_to_string(&marker) {
                        if let Ok(pid) = bytes.parse::<u32>() {
                            break pid;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("successor acknowledgement");
            let deadline = Instant::now() + Duration::from_secs(3);
            while initial.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "initial member reap deadline");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(linux_process_running(successor));
            if on_drop {
                terminate_tokio_on_drop(process, ProcessOwnership::Owned);
            } else {
                terminate_tokio(
                    &mut process,
                    ProcessOwnership::Owned,
                    Duration::from_millis(100),
                )
                .await
                .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while linux_process_running(successor) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("successor must retire before fixture control pipe closes");
            drop(control);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn review_child_churn_does_not_retain_historical_proc_descriptors() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let mut process =
            ManagedProcess::spawn_owned(Command::new("/bin/sh").args(["-c", "sleep 30"])).unwrap();
        let anchor = process.group.as_ref().unwrap().anchor.as_ref().unwrap();
        let pipe = std::fs::read_link(format!(
            "/proc/self/fd/{}",
            anchor.stdin.as_ref().unwrap().as_raw_fd()
        ))
        .unwrap();
        let mut retired = Vec::new();
        for _ in 0..64 {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", "exit 0"])
                .process_group(process.id() as i32);
            let mut child = command.spawn().unwrap();
            retired.push(child.id());
            assert!(process.try_wait().unwrap().is_none());
            child.wait().unwrap();
        }
        let retained = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|path| {
                retired.iter().any(|pid| {
                    path.to_string_lossy()
                        .starts_with(&format!("/proc/{pid}/stat"))
                })
            })
            .count();
        assert_eq!(
            retained, 0,
            "ownership must not retain one FD per historical member"
        );
        let pipes = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter(|path| path == &pipe)
            .count();
        assert_eq!(
            pipes, 1,
            "exactly one parent-owned anchor pipe remains after child churn"
        );
        let mut probe = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .process_group(process.id() as i32)
            .spawn()
            .unwrap();
        process.terminate(Duration::from_millis(100)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while probe.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "group child must retire after churn"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_process_is_not_group_owned() {
        let child = Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .spawn()
            .expect("test command should spawn");
        let process = ManagedProcess::attach_external(child);
        assert_eq!(process.ownership(), ProcessOwnership::External);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_process_group_is_terminated_and_child_is_reaped() {
        let mut process =
            ManagedProcess::spawn_owned(Command::new("sh").arg("-c").arg("sleep 30 & wait"))
                .expect("test process should spawn");

        process
            .terminate(Duration::from_secs(2))
            .expect("owned process group should terminate");
        assert!(process
            .try_wait()
            .expect("child should be reapable")
            .is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_group_kills_stubborn_descendant_after_leader_exits() {
        let fixture = crate::test_support::TestDir::new("managed-descendant");
        let marker = fixture.path().join("descendant.pid");
        let script = format!(
            "sh -c 'trap \"\" TERM; exec sleep 30' & echo $! > '{}'; exit 0",
            marker.display()
        );
        let mut process = ManagedProcess::spawn_owned(Command::new("sh").arg("-c").arg(script))
            .expect("test process should spawn");

        let deadline = Instant::now() + Duration::from_secs(2);
        let descendant = loop {
            if let Ok(pid) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = pid.trim().parse::<u32>() {
                    break pid;
                }
            }
            assert!(Instant::now() < deadline, "descendant pid was not written");
            std::thread::sleep(Duration::from_millis(10));
        };

        process
            .terminate(Duration::from_millis(100))
            .expect("owned process group should terminate");
        let deadline = Instant::now() + Duration::from_secs(2);
        while linux_process_running(descendant) {
            assert!(Instant::now() < deadline, "owned descendant should retire");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(target_os = "linux")]
    fn linux_process_running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .map(|(_, rest)| rest.trim_start().chars().next().unwrap())
            })
            .is_some_and(|state| !matches!(state, 'Z' | 'X'))
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tokio_owned_group_retires_descendant_after_leader_was_reaped() {
        assert_tokio_descendant_retirement(false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tokio_drop_retires_descendant_after_leader_was_reaped() {
        assert_tokio_descendant_retirement(true).await;
    }

    #[cfg(target_os = "linux")]
    async fn assert_tokio_descendant_retirement(on_drop: bool) {
        let fixture = crate::test_support::TestDir::new("tokio-descendant");
        let marker = fixture.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("sh");
        command.args([
            "-c",
            &format!(
                "sh -c 'trap \"\" TERM; exec sleep 30' & echo $! > '{}'; exit 0",
                marker.display()
            ),
        ]);
        configure_owned_tokio(&mut command);
        let mut process = ManagedTokioProcess::from_owned_child(command.spawn().unwrap()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(process.id().is_none(), "Tokio releases Child.id after reap");
        let descendant: u32 = std::fs::read_to_string(marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            linux_process_running(descendant),
            "descendant must be alive before shutdown"
        );
        if on_drop {
            terminate_tokio_on_drop(process, ProcessOwnership::Owned);
        } else {
            terminate_tokio(
                &mut process,
                ProcessOwnership::Owned,
                Duration::from_millis(100),
            )
            .await
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while linux_process_running(descendant) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual owned descendant retirement");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_anchor_does_not_release_identity_before_final_group_signal() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        for on_drop in [false, true] {
            let mut process =
                ManagedProcess::spawn_owned(Command::new("/bin/sh").args(["-c", "exit 0"]))
                    .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while process.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let anchor = process.group.as_mut().unwrap().anchor.as_mut().unwrap();
            let anchor_pid = anchor.id();
            anchor.kill().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while linux_process_running(anchor_pid) {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                std::path::Path::new(&format!("/proc/{anchor_pid}")).exists(),
                "dead anchor must remain unreaped"
            );
            let fixture = crate::test_support::TestDir::new("dead-anchor-probe");
            let ready = fixture.path().join("ready");
            let mut probe = Command::new("/bin/sh")
                .args([
                    "-c",
                    "trap '' TERM HUP INT; printf ready > \"$1\"; exec sleep 30",
                    "synthetic-probe",
                ])
                .arg(&ready)
                .process_group(process.id() as i32)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !ready.exists() {
                assert!(Instant::now() < deadline, "probe trap acknowledgement");
                std::thread::sleep(Duration::from_millis(5));
            }
            if on_drop {
                drop(process);
            } else {
                process.terminate(Duration::from_millis(100)).unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(status) = probe.try_wait().unwrap() {
                    assert_eq!(status.signal(), Some(9));
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "final group signal must retire probe"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                !std::path::Path::new(&format!("/proc/{anchor_pid}")).exists(),
                "anchor reaped after final group signal"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn external_tokio_shutdown_reaps_only_the_child_not_its_group() {
        struct FixtureGroup(OwnedGroup);
        impl Drop for FixtureGroup {
            fn drop(&mut self) {
                let _ = self.0.terminate(true);
            }
        }
        let fixture = crate::test_support::TestDir::new("external-descendant");
        let marker = fixture.path().join("descendant.pid");
        let mut command = tokio::process::Command::new("sh");
        command.args([
            "-c",
            &format!("sleep 30 & echo $! > '{}'; wait", marker.display()),
        ]);
        // The fixture owns cleanup, not the externally attached runtime wrapper.
        configure_owned_tokio(&mut command);
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        let mut cleanup = FixtureGroup(OwnedGroup::new(pid).unwrap());
        let mut process = ManagedTokioProcess::attach_external(child);
        let descendant = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(bytes) = std::fs::read_to_string(&marker) {
                    if let Ok(pid) = bytes.trim().parse::<u32>() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("external fixture descendant acknowledgement");
        terminate_tokio(
            &mut process,
            ProcessOwnership::External,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(process.id().is_none(), "external child was reaped");
        drop(process);
        assert!(
            linux_process_running(descendant),
            "external descendant must not be group-killed by shutdown or drop"
        );
        cleanup.0.terminate(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while linux_process_running(descendant) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture-owned descendant cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn external_process_is_directly_terminated_and_reaped() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("exec sleep 30")
            .spawn()
            .expect("test process should spawn");
        let mut process = ManagedProcess::attach_external(child);

        process
            .terminate(Duration::from_secs(2))
            .expect("external child should terminate");
        assert!(process
            .try_wait()
            .expect("child should be reapable")
            .is_some());
    }
}
