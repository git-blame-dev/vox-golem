//! Bounded, argv-only access to the default WSL distribution.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 64 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const ETXTBSY_RETRY_COUNT: usize = 20;
#[cfg(unix)]
const ETXTBSY_RETRY_DELAY: Duration = Duration::from_millis(10);
const OWNED_PROCESS_WRAPPER: &str = r#"
mode=$0
child=$1
shift
exec 3<&0
if [ "$mode" = quiet ]; then
    setsid "$child" "$@" </dev/null >/dev/null 2>/dev/null &
elif [ "$mode" = capture ]; then
    setsid "$child" "$@" </dev/null &
else
    exit 2
fi
pid=$!
terminate_group() {
    kill -TERM -"$pid" 2>/dev/null || true
    count=0
    while kill -0 -"$pid" 2>/dev/null && [ "$count" -lt 10 ]; do
        sleep 0.05
        count=$((count + 1))
    done
    kill -KILL -"$pid" 2>/dev/null || true
}
trap 'terminate_group; exit 143' HUP INT TERM
( IFS= read -r _ <&3 || true; terminate_group ) &
monitor=$!
exec 3<&-
wait "$pid"
status=$?
kill "$monitor" 2>/dev/null || true
wait "$monitor" 2>/dev/null || true
kill -KILL -"$pid" 2>/dev/null || true
exit "$status"
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslCommandSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
}

impl WslCommandSpec {
    pub fn new(
        executable: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            executable: executable.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.args);
        command
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslLaunchSpec {
    pub command: WslCommandSpec,
}

impl WslLaunchSpec {
    pub fn to_command(&self, password: Option<&str>) -> Command {
        let mut command = self.command.to_command();
        configure_password_environment(&mut command, password, std::env::var_os("WSLENV"));
        command
    }

    pub fn to_tokio_command(&self, password: Option<&str>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.command.executable);
        command.args(&self.command.args);
        configure_password_environment(&mut command, password, std::env::var_os("WSLENV"));
        command
    }
}

trait CommandEnvironment {
    fn set_env(&mut self, key: &str, value: &OsStr);
}

impl CommandEnvironment for Command {
    fn set_env(&mut self, key: &str, value: &OsStr) {
        self.env(key, value);
    }
}

impl CommandEnvironment for tokio::process::Command {
    fn set_env(&mut self, key: &str, value: &OsStr) {
        self.env(key, value);
    }
}

fn configure_password_environment(
    command: &mut impl CommandEnvironment,
    password: Option<&str>,
    existing_wslenv: Option<OsString>,
) {
    let Some(password) = password else {
        return;
    };
    command.set_env("OPENCODE_SERVER_PASSWORD", OsStr::new(password));
    let mut entries = existing_wslenv
        .as_deref()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    entries.retain(|entry| entry.split('/').next() != Some("OPENCODE_SERVER_PASSWORD"));
    entries.push(String::from("OPENCODE_SERVER_PASSWORD"));
    command.set_env("WSLENV", OsStr::new(&entries.join(":")));
}

#[derive(Debug, Clone)]
pub struct WslRunner {
    wsl: PathBuf,
    timeout: Duration,
    max_output: usize,
}

impl Default for WslRunner {
    fn default() -> Self {
        Self::new("wsl.exe")
    }
}

impl WslRunner {
    pub fn new(wsl: impl Into<PathBuf>) -> Self {
        Self {
            wsl: wsl.into(),
            timeout: DEFAULT_TIMEOUT,
            max_output: MAX_OUTPUT,
        }
    }

    pub fn with_limits(mut self, timeout: Duration, max_output: usize) -> Self {
        self.timeout = timeout;
        self.max_output = max_output;
        self
    }

    pub fn default_home(&self) -> Result<String, WslError> {
        let home = text(
            self.run(["--exec", "sh", "-c", "printf %s \"$HOME\""])?,
            "home",
        )
        .map_err(|error| match error {
            WslError::Failed(_) => WslError::HomeUnavailable,
            error => error,
        })?;
        if home.starts_with('/') {
            Ok(home)
        } else {
            Err(WslError::HomeUnavailable)
        }
    }

    pub fn linux_path_to_windows(&self, path: &Path) -> Result<PathBuf, WslError> {
        text(
            self.run(["--exec", "wslpath", "-w", &path.to_string_lossy()])?,
            "path",
        )
        .map(PathBuf::from)
    }

    fn linux_path_string_to_windows(&self, path: &str) -> Result<PathBuf, WslError> {
        text(self.run(["--exec", "wslpath", "-w", path])?, "path").map(PathBuf::from)
    }

    pub fn resolve_auth_path(&self, linux_path: Option<&Path>) -> Result<PathBuf, WslError> {
        let path = match linux_path {
            Some(path) => {
                self.check_default_distribution()?;
                path.to_string_lossy().into_owned()
            }
            None => {
                let home = self.default_home().map_err(default_distribution_error)?;
                join_linux_home(&home, ".local/share/opencode/auth.json")
            }
        };
        self.linux_path_string_to_windows(&path)
    }

    pub fn discover_opencode(&self, explicit: Option<&Path>) -> Result<PathBuf, WslError> {
        if let Some(path) = explicit {
            self.check_default_distribution()?;
            self.run(["--exec", "test", "-x", &path.to_string_lossy()])
                .map_err(missing_opencode_error)?;
            return Ok(path.to_path_buf());
        }
        let home = self.default_home().map_err(default_distribution_error)?;
        let candidate = join_linux_home(&home, ".opencode/bin/opencode");
        match self.run(["--exec", "test", "-x", &candidate]) {
            Ok(_) => return Ok(PathBuf::from(candidate)),
            Err(WslError::Failed(_)) => {}
            Err(error) => return Err(error),
        }
        text(
            self.run(["--exec", "sh", "-lc", "command -v opencode"])
                .map_err(missing_opencode_error)?,
            "OpenCode executable",
        )
        .map(PathBuf::from)
        .map_err(missing_opencode_error)
    }

    fn check_default_distribution(&self) -> Result<(), WslError> {
        self.run(["--exec", "true"])
            .map(|_| ())
            .map_err(default_distribution_error)
    }

    pub fn launch_opencode(&self, executable: &Path, args: &[String]) -> WslLaunchSpec {
        let mut command = WslCommandSpec::new(
            &self.wsl,
            [
                "--exec",
                "sh",
                "-c",
                OWNED_PROCESS_WRAPPER,
                "quiet",
                &executable.to_string_lossy(),
            ],
        );
        command.args.extend(args.iter().cloned());
        WslLaunchSpec { command }
    }

    fn run<I, S>(&self, args: I) -> Result<CapturedOutput, WslError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_owned())
            .collect::<Vec<_>>();
        if args.len() < 2 || args[0] != "--exec" {
            return Err(WslError::Failed("command"));
        }
        let mut owned_args = vec![
            String::from("--exec"),
            String::from("sh"),
            String::from("-c"),
            String::from(OWNED_PROCESS_WRAPPER),
            String::from("capture"),
            args[1].clone(),
        ];
        owned_args.extend(args[2..].iter().cloned());
        let mut command = Command::new(&self.wsl);
        command
            .args(owned_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::managed_process::configure_owned(&mut command);
        let deadline = Instant::now() + self.timeout;
        let mut child = spawn_command(&mut command).map_err(|_| WslError::Unavailable)?;
        let pid = child.id();
        let stdout_pipe = child.stdout.take().ok_or(WslError::Failed("output"))?;
        let stderr_pipe = child.stderr.take().ok_or(WslError::Failed("output"))?;
        let max_output = self.max_output;
        let (stdout_sender, stdout_receiver) = mpsc::channel();
        let (stderr_sender, stderr_receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = stdout_sender.send(read_bounded(stdout_pipe, max_output));
        });
        thread::spawn(move || {
            let _ = stderr_sender.send(read_bounded(stderr_pipe, max_output));
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => {
                    stop_run_child(&mut child, pid);
                    return Err(WslError::Failed("wait"));
                }
            }
            if Instant::now() >= deadline {
                stop_run_child(&mut child, pid);
                return Err(WslError::Timeout);
            }
            thread::sleep(Duration::from_millis(5));
        };
        let stdout = match receive_output(stdout_receiver, deadline) {
            Ok(output) => output,
            Err(error) => {
                stop_run_child(&mut child, pid);
                return Err(error);
            }
        };
        let stderr = match receive_output(stderr_receiver, deadline) {
            Ok(output) => output,
            Err(error) => {
                stop_run_child(&mut child, pid);
                return Err(error);
            }
        };
        if stdout.overflow || stderr.overflow {
            return Err(WslError::OutputLimit);
        }
        if !status.success() {
            return Err(WslError::Failed("command"));
        }
        Ok(CapturedOutput {
            stdout: stdout.bytes,
        })
    }
}

fn spawn_command(command: &mut Command) -> std::io::Result<std::process::Child> {
    #[cfg(unix)]
    for attempt in 0..ETXTBSY_RETRY_COUNT {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(26) && attempt + 1 < ETXTBSY_RETRY_COUNT => {
                thread::sleep(ETXTBSY_RETRY_DELAY);
            }
            result => return result,
        }
    }

    #[cfg(not(unix))]
    return command.spawn();

    #[cfg(unix)]
    unreachable!("the bounded command retry loop always returns")
}

#[derive(Debug)]
struct CapturedOutput {
    stdout: Vec<u8>,
}

struct BoundedBytes {
    bytes: Vec<u8>,
    overflow: bool,
}

fn read_bounded(mut input: impl Read, limit: usize) -> std::io::Result<BoundedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(4096));
    let mut overflow = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        overflow |= read > remaining;
    }
    Ok(BoundedBytes { bytes, overflow })
}

fn receive_output(
    receiver: mpsc::Receiver<std::io::Result<BoundedBytes>>,
    deadline: Instant,
) -> Result<BoundedBytes, WslError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(WslError::Timeout);
    }
    receiver
        .recv_timeout(remaining)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => WslError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => WslError::Failed("output"),
        })?
        .map_err(|_| WslError::Failed("output"))
}

fn stop_run_child(child: &mut std::process::Child, _pid: u32) {
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_millis(750);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    #[cfg(unix)]
    let _ = crate::managed_process::terminate_group(_pid, true);
    let _ = child.kill();
    let _ = child.wait();
}

fn text(output: CapturedOutput, what: &'static str) -> Result<String, WslError> {
    let value = String::from_utf8(output.stdout)
        .map_err(|_| WslError::Failed(what))?
        .trim()
        .to_owned();
    if value.is_empty() {
        Err(WslError::Failed(what))
    } else {
        Ok(value)
    }
}

fn join_linux_home(home: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        home.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

fn default_distribution_error(error: WslError) -> WslError {
    match error {
        WslError::Failed("command") => WslError::DefaultDistributionUnavailable,
        error => error,
    }
}

fn missing_opencode_error(error: WslError) -> WslError {
    match error {
        WslError::Failed(_) => WslError::OpenCodeUnavailable,
        error => error,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WslError {
    Unavailable,
    DefaultDistributionUnavailable,
    HomeUnavailable,
    OpenCodeUnavailable,
    Timeout,
    OutputLimit,
    Failed(&'static str),
}

impl std::fmt::Display for WslError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("WSL is unavailable"),
            Self::DefaultDistributionUnavailable => {
                formatter.write_str("WSL default distribution is unavailable")
            }
            Self::HomeUnavailable => formatter.write_str("WSL home directory is unavailable"),
            Self::OpenCodeUnavailable => {
                formatter.write_str("OpenCode is unavailable in the default WSL distribution")
            }
            Self::Timeout => formatter.write_str("WSL command timed out"),
            Self::OutputLimit => formatter.write_str("WSL command output exceeded the limit"),
            Self::Failed(what) => write!(formatter, "WSL {what} failed"),
        }
    }
}

impl std::error::Error for WslError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_argv_without_interpolation() {
        let spec = WslCommandSpec::new("wsl.exe", ["--exec", "wslpath", "-w", "a; secret"]);
        assert_eq!(
            spec.to_command()
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["--exec", "wslpath", "-w", "a; secret"]
        );
    }

    #[test]
    fn launch_propagates_password_without_argv_exposure() {
        let spec =
            WslRunner::new("wsl.exe").launch_opencode(Path::new("opencode"), &["serve".into()]);
        assert_eq!(&spec.command.args[..3], ["--exec", "sh", "-c"]);
        assert_eq!(&spec.command.args[4..], ["quiet", "opencode", "serve"]);
        let command = spec.to_command(Some("secret"));
        assert_eq!(command_env(&command, "OPENCODE_SERVER_PASSWORD"), "secret");
        assert!(command_env(&command, "WSLENV").contains("OPENCODE_SERVER_PASSWORD"));
        assert!(!spec.command.args.iter().any(|arg| arg == "secret"));
    }

    #[cfg(unix)]
    #[test]
    fn owned_launch_closes_control_pipe_and_terminates_only_its_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let suffix = format!("{}-{:?}", std::process::id(), Instant::now());
        let fake_wsl = std::env::temp_dir().join(format!("voxgolem-fake-wsl-{suffix}"));
        let marker = std::env::temp_dir().join(format!("voxgolem-owned-pid-{suffix}"));
        std::fs::write(
            &fake_wsl,
            "#!/bin/sh\n[ \"$1\" = --exec ] || exit 2\nshift\nexec \"$@\"\n",
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&fake_wsl, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let launch = WslRunner::new(&fake_wsl).launch_opencode(
            Path::new("/bin/sh"),
            &[
                String::from("-c"),
                String::from("printf %s $$ > \"$1\"; exec sleep 30"),
                String::from("owned-server"),
                marker.to_string_lossy().into_owned(),
            ],
        );
        let mut command = launch.to_command(None);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut owned = command.spawn().expect("owned WSL process");
        let mut unrelated = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("unrelated process");
        let deadline = Instant::now() + Duration::from_secs(2);
        let owned_pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&marker) {
                break pid.trim().parse::<u32>().expect("owned Linux PID");
            }
            assert!(Instant::now() < deadline, "owned PID was not written");
            thread::sleep(Duration::from_millis(10));
        };

        drop(owned.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        let exited = loop {
            if owned.try_wait().expect("owned process status").is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        if !exited {
            let _ = owned.kill();
            let _ = owned.wait();
        }
        let owned_alive = Command::new("/usr/bin/kill")
            .args(["-0", &owned_pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let unrelated_alive = unrelated
            .try_wait()
            .expect("unrelated process status")
            .is_none();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        let _ = std::fs::remove_file(&fake_wsl);
        let _ = std::fs::remove_file(&marker);

        assert!(exited, "control-pipe closure must stop the WSL wrapper");
        assert!(!owned_alive, "owned Linux process must be terminated");
        assert!(unrelated_alive, "unrelated Linux process must survive");
    }

    #[test]
    fn password_environment_preserves_entries_without_duplication() {
        let mut command = Command::new("wsl.exe");
        configure_password_environment(
            &mut command,
            Some("secret"),
            Some(OsString::from("EXISTING/u:OPENCODE_SERVER_PASSWORD/u")),
        );
        assert_eq!(
            command_env(&command, "WSLENV"),
            "EXISTING/u:OPENCODE_SERVER_PASSWORD"
        );
    }

    #[test]
    fn errors_are_stable_and_redacted() {
        let error = WslError::Failed("command");
        assert_eq!(error.to_string(), "WSL command failed");
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn linux_home_paths_use_posix_separators() {
        assert_eq!(
            join_linux_home("/home/test", ".local/share/opencode/auth.json"),
            "/home/test/.local/share/opencode/auth.json"
        );
        assert_eq!(
            join_linux_home("/home/test/", "/.opencode/bin/opencode"),
            "/home/test/.opencode/bin/opencode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn absent_default_distribution_has_a_distinct_error() {
        let runner = WslRunner::new("/bin/false");
        assert_eq!(
            runner.resolve_auth_path(None).unwrap_err(),
            WslError::DefaultDistributionUnavailable
        );
        assert_eq!(
            runner
                .resolve_auth_path(Some(Path::new("/custom/auth.json")))
                .unwrap_err(),
            WslError::DefaultDistributionUnavailable
        );
        assert_eq!(
            runner
                .discover_opencode(Some(Path::new("/custom/opencode")))
                .unwrap_err(),
            WslError::DefaultDistributionUnavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_home_has_a_distinct_error() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-empty-home-{}",
            std::process::id()
        ));
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let result = WslRunner::new(&script).resolve_auth_path(None);
        let _ = std::fs::remove_file(&script);

        assert_eq!(result.unwrap_err(), WslError::HomeUnavailable);
    }

    #[cfg(unix)]
    #[test]
    fn home_must_be_an_absolute_posix_path() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-relative-home-{}",
            std::process::id()
        ));
        for home in ["relative/home", r"C:\Users\test"] {
            std::fs::write(&script, format!("#!/bin/sh\nprintf '%s' '{home}'\n"))
                .expect("fake WSL script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("fake WSL permissions");
            assert_eq!(
                WslRunner::new(&script).resolve_auth_path(None).unwrap_err(),
                WslError::HomeUnavailable
            );
        }
        let _ = std::fs::remove_file(&script);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_paths_do_not_require_home_resolution() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-explicit-paths-{}",
            std::process::id()
        ));
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$6\" = true ]; then exit 0; fi\nif [ \"$6\" = wslpath ]; then printf 'C:\\\\auth.json'; exit 0; fi\nif [ \"$6\" = test ] && [ \"$7\" = -x ] && [ \"$8\" = /custom/opencode ]; then exit 0; fi\nexit 1\n",
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let runner = WslRunner::new(&script);
        let auth = runner.resolve_auth_path(Some(Path::new("/custom/auth.json")));
        let opencode = runner.discover_opencode(Some(Path::new("/custom/opencode")));
        let _ = std::fs::remove_file(&script);

        assert_eq!(auth.unwrap(), PathBuf::from(r"C:\auth.json"));
        assert_eq!(opencode.unwrap(), PathBuf::from("/custom/opencode"));
    }

    #[cfg(unix)]
    #[test]
    fn missing_explicit_opencode_preserves_the_failure_boundary() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!("voxgolem-fake-wsl-{}", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$6\" = true ]; then exit 0; fi\nexit 1\n",
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let result =
            WslRunner::new(&script).discover_opencode(Some(Path::new("/missing/opencode")));
        let _ = std::fs::remove_file(&script);

        assert_eq!(result.unwrap_err(), WslError::OpenCodeUnavailable);
        assert_eq!(missing_opencode_error(WslError::Timeout), WslError::Timeout);
    }

    #[cfg(unix)]
    #[test]
    fn missing_standard_and_path_opencode_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-path-miss-{}",
            std::process::id()
        ));
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$6\" = sh ] && [ \"$8\" = 'printf %s \"$HOME\"' ]; then printf /home/test; exit 0; fi\nexit 1\n",
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let result = WslRunner::new(&script).discover_opencode(None);
        let _ = std::fs::remove_file(&script);

        assert_eq!(result.unwrap_err(), WslError::OpenCodeUnavailable);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_is_bounded() {
        let script = fake_wsl_forwarder("timeout");
        let runner = WslRunner::new(&script).with_limits(Duration::from_millis(20), MAX_OUTPUT);
        let started = Instant::now();
        assert_eq!(
            runner.run(["--exec", "sh", "-c", "sleep 1"]).unwrap_err(),
            WslError::Timeout
        );
        let _ = std::fs::remove_file(&script);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn output_is_bounded_while_the_process_runs() {
        let script = fake_wsl_forwarder("output-limit");
        let runner = WslRunner::new(&script).with_limits(Duration::from_secs(1), 16);
        assert_eq!(
            runner
                .run(["--exec", "sh", "-c", "printf 12345678901234567"])
                .unwrap_err(),
            WslError::OutputLimit
        );
        let _ = std::fs::remove_file(&script);
    }

    #[cfg(unix)]
    #[test]
    fn output_drain_timeout_terminates_a_descriptor_holding_descendant() {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-retained-output-{}",
            std::process::id()
        ));
        let marker = script.with_extension("pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s' $! > '{}'\nexit 0\n",
                marker.display()
            ),
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        let runner = WslRunner::new(&script).with_limits(Duration::from_millis(30), MAX_OUTPUT);

        assert_eq!(
            runner.run(["--exec", "true"]).unwrap_err(),
            WslError::Timeout
        );
        let pid = std::fs::read_to_string(&marker)
            .expect("descendant PID")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant PID");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(&marker);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[cfg(unix)]
    fn fake_wsl_forwarder(name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!(
            "voxgolem-fake-wsl-forwarder-{name}-{}",
            std::process::id()
        ));
        std::fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = --exec ] || exit 2\nshift\nexec \"$@\"\n",
        )
        .expect("fake WSL script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("fake WSL permissions");
        script
    }

    fn command_env(command: &Command, key: &str) -> String {
        command
            .get_envs()
            .find(|(name, _)| *name == key)
            .and_then(|(_, value)| value)
            .expect("environment value")
            .to_string_lossy()
            .into_owned()
    }
}
