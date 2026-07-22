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

pub struct ManagedProcess {
    child: Child,
    ownership: ProcessOwnership,
}

/// Configure a `tokio::process::Command` exactly like `spawn_owned` configures
/// a standard command.  Kept here so all runtimes share the ownership rule.
pub fn configure_owned_tokio(command: &mut tokio::process::Command) {
    platform::configure_owned_tokio(command);
}

pub fn configure_owned(command: &mut Command) {
    platform::configure_owned(command);
}

pub fn terminate_group(pid: u32, force: bool) -> io::Result<()> {
    platform::terminate_group(pid, force)
}

/// Terminate and reap a tokio child, signalling its owned group on Unix.
pub async fn terminate_tokio(
    child: &mut tokio::process::Child,
    ownership: ProcessOwnership,
    timeout: Duration,
) -> io::Result<()> {
    let pid = child.id();
    if ownership == ProcessOwnership::Owned {
        let pid = pid.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "managed process has no PID")
        })?;
        if child.try_wait()?.is_some() {
            return platform::terminate_group(pid, true);
        }
        platform::terminate_group(pid, false)?;
        platform::kill_tokio_child(child)?;
        let exited = tokio::time::timeout(timeout, child.wait()).await.is_ok();
        platform::terminate_group(pid, true)?;
        platform::kill_tokio_child(child)?;
        if exited {
            return Ok(());
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
    if tokio::time::timeout(timeout, child.wait()).await.is_ok() {
        return Ok(());
    }
    child.start_kill()?;
    tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "managed process did not exit"))?
        .map(|_| ())
}

impl ManagedProcess {
    /// Spawn a child in a new process group on Unix.
    pub fn spawn_owned(command: &mut Command) -> io::Result<Self> {
        platform::configure_owned(command);
        Ok(Self {
            child: command.spawn()?,
            ownership: ProcessOwnership::Owned,
        })
    }

    /// Manage an already-created child without claiming its process group.
    pub fn attach_external(child: Child) -> Self {
        Self {
            child,
            ownership: ProcessOwnership::External,
        }
    }

    pub fn ownership(&self) -> ProcessOwnership {
        self.ownership
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Stop the child, escalating from a graceful request to a forced kill.
    /// Owned children use group termination; externally attached children use
    /// direct child termination only.
    pub fn terminate(&mut self, timeout: Duration) -> io::Result<()> {
        if self.ownership == ProcessOwnership::Owned {
            platform::terminate_group(self.id(), false)?;
            platform::kill_child(&mut self.child)?;
            // Keep the group as the ownership boundary even if its leader
            // exits during the grace period.
            let _ = wait_until_exited(&mut self.child, timeout)?;
            // If wait reaped the leader, a surviving descendant is still a
            // member of this PGID; Linux cannot recycle that PGID while the
            // member exists.  If no member survives, there is no owned target
            // to signal.  The descendant-retention case is exercised below.
            platform::terminate_group(self.id(), true)?;
            platform::kill_child(&mut self.child)?;
            return wait_until_exited(&mut self.child, timeout)?
                .map(|_| ())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "managed process did not exit")
                });
        }

        self.child.kill()?;
        if wait_until_exited(&mut self.child, timeout)?.is_some() {
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
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if self.ownership == ProcessOwnership::Owned {
            // Reaping the leader does not release descendants from its group.
            // Signal the ownership boundary even when the leader is gone.
            let _ = platform::terminate_group(self.id(), true);
            if self.try_wait().ok().flatten().is_none() {
                let _ = self.terminate(Duration::from_secs(2));
            }
        } else if self.try_wait().ok().flatten().is_none() {
            let _ = self.terminate(Duration::from_secs(2));
        }
    }
}

fn wait_until_exited(
    child: &mut Child,
    timeout: Duration,
) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
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

    #[cfg(unix)]
    #[test]
    fn external_process_is_not_group_owned() {
        let child = Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .spawn()
            .expect("test command should spawn");
        let process = ManagedProcess::attach_external(child);
        assert_eq!(process.ownership(), ProcessOwnership::External);
    }

    #[cfg(unix)]
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
        let marker =
            std::env::temp_dir().join(format!("managed-process-{}.pid", std::process::id()));
        let script = format!("sleep 30 & echo $! > {}; exit 0", marker.display());
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
        let status = loop {
            let status = Command::new("/usr/bin/kill")
                .args(["-0", &descendant.to_string()])
                .status()
                .expect("kill should run");
            if !status.success() || Instant::now() >= deadline {
                break status;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = std::fs::remove_file(marker);
        assert!(!status.success(), "owned descendant should be gone");
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
