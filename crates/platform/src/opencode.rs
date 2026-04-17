use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodePrompt {
    text: String,
}

impl OpencodePrompt {
    pub fn new(text: impl Into<String>) -> Result<Self, OpencodePromptError> {
        let text = text.into();

        if text.trim().is_empty() {
            return Err(OpencodePromptError::EmptyPrompt);
        }

        Ok(Self { text })
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodePromptError {
    EmptyPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeCommandSpec {
    executable_path: PathBuf,
    output_format: OpencodeOutputFormat,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeRunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeJsonRunResult {
    pub events: Vec<OpencodeJsonEvent>,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpencodeJsonEvent {
    Text {
        text: String,
    },
    Error {
        name: String,
        message: String,
    },
    ToolUse {
        tool: String,
        status: OpencodeToolUseStatus,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeToolUseStatus {
    Completed,
    Error,
}

#[derive(Debug)]
pub enum OpencodeJsonRunError {
    Io(std::io::Error),
    InvalidJsonLine { line_number: usize, details: String },
}

impl std::fmt::Display for OpencodeJsonRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidJsonLine {
                line_number,
                details,
            } => write!(
                formatter,
                "invalid OpenCode JSON event on line {line_number}: {details}"
            ),
        }
    }
}

impl std::error::Error for OpencodeJsonRunError {}

impl OpencodeRunResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeOutputFormat {
    Default,
    Json,
}

impl OpencodeCommandSpec {
    pub fn new(executable_path: impl Into<PathBuf>, prompt: OpencodePrompt) -> Self {
        Self {
            executable_path: executable_path.into(),
            output_format: OpencodeOutputFormat::Default,
            args: vec![String::from("run"), prompt.text().to_string()],
        }
    }

    pub fn with_output_format(mut self, output_format: OpencodeOutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.executable_path);

        if self.output_format == OpencodeOutputFormat::Json {
            command.args(["run", "--format", "json"]);
            command.args(&self.args[1..]);
        } else {
            command.args(&self.args);
        }

        command
    }
}

pub fn run_opencode(spec: &OpencodeCommandSpec) -> std::io::Result<OpencodeRunResult> {
    let output = spec.to_command().output()?;

    Ok(OpencodeRunResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
    })
}

pub fn run_opencode_json(
    spec: &OpencodeCommandSpec,
) -> Result<OpencodeJsonRunResult, OpencodeJsonRunError> {
    let output = spec
        .to_command()
        .output()
        .map_err(OpencodeJsonRunError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    Ok(OpencodeJsonRunResult {
        events: parse_json_events(&stdout)?,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
    })
}

fn parse_json_events(stdout: &str) -> Result<Vec<OpencodeJsonEvent>, OpencodeJsonRunError> {
    let mut events = Vec::new();

    for (line_index, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        let raw_event = serde_json::from_str::<RawJsonEvent>(trimmed).map_err(|error| {
            OpencodeJsonRunError::InvalidJsonLine {
                line_number: line_index + 1,
                details: error.to_string(),
            }
        })?;

        match raw_event {
            RawJsonEvent::Text { part, .. } => {
                let text = part.text.trim();

                if !text.is_empty() {
                    events.push(OpencodeJsonEvent::Text {
                        text: text.to_string(),
                    });
                }
            }
            RawJsonEvent::Error { error, .. } => {
                let message = error
                    .data
                    .as_ref()
                    .and_then(|data| data.message.as_deref())
                    .unwrap_or(&error.name)
                    .to_string();

                events.push(OpencodeJsonEvent::Error {
                    name: error.name,
                    message,
                });
            }
            RawJsonEvent::ToolUse { part, .. } => match part.state {
                RawToolState::Completed { title, output } => {
                    let detail = if title.trim().is_empty() {
                        output.trim().to_string()
                    } else {
                        title
                    };

                    if !detail.is_empty() {
                        events.push(OpencodeJsonEvent::ToolUse {
                            tool: part.tool,
                            status: OpencodeToolUseStatus::Completed,
                            detail,
                        });
                    }
                }
                RawToolState::Error { error } => {
                    let detail = error.trim();

                    if !detail.is_empty() {
                        events.push(OpencodeJsonEvent::ToolUse {
                            tool: part.tool,
                            status: OpencodeToolUseStatus::Error,
                            detail: detail.to_string(),
                        });
                    }
                }
            },
            RawJsonEvent::Other => {}
        }
    }

    Ok(events)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawJsonEvent {
    #[serde(rename = "text")]
    Text {
        part: RawTextPart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "error")]
    Error {
        error: RawErrorPayload,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        part: RawToolUsePart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawTextPart {
    text: String,
}

#[derive(Debug, Deserialize)]
struct RawErrorPayload {
    name: String,
    data: Option<RawErrorData>,
}

#[derive(Debug, Deserialize)]
struct RawErrorData {
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawToolUsePart {
    tool: String,
    state: RawToolState,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
enum RawToolState {
    #[serde(rename = "completed")]
    Completed { title: String, output: String },
    #[serde(rename = "error")]
    Error { error: String },
}

#[cfg(test)]
mod tests {
    use super::{
        run_opencode, run_opencode_json, OpencodeCommandSpec, OpencodeJsonEvent,
        OpencodeJsonRunError, OpencodeOutputFormat, OpencodePrompt, OpencodePromptError,
        OpencodeToolUseStatus,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();

            let path = std::env::temp_dir().join(format!(
                "voxgolem-opencode-tests-{}-{stamp}",
                std::process::id()
            ));

            fs::create_dir_all(&path).expect("temporary test directory should be creatable");

            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn rejects_blank_prompts() {
        assert_eq!(
            OpencodePrompt::new("   \n\t "),
            Err(OpencodePromptError::EmptyPrompt)
        );
    }

    #[test]
    fn preserves_non_empty_prompt_text() {
        let prompt = OpencodePrompt::new("summarize the latest transcript")
            .expect("non-empty prompt should be accepted");

        assert_eq!(prompt.text(), "summarize the latest transcript");
    }

    #[test]
    fn builds_run_command_spec() {
        let prompt = OpencodePrompt::new("open the release checklist")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new("C:/Program Files/OpenCode/opencode.exe", prompt);

        assert_eq!(
            spec.executable_path(),
            Path::new("C:/Program Files/OpenCode/opencode.exe")
        );
        assert_eq!(
            spec.args(),
            &[
                String::from("run"),
                String::from("open the release checklist")
            ]
        );
    }

    #[test]
    fn keeps_shell_like_characters_inside_single_argument() {
        let prompt = OpencodePrompt::new("say hello && remove nothing")
            .expect("shell-like prompt text should still be accepted");
        let spec = OpencodeCommandSpec::new("opencode.exe", prompt);
        let command = spec.to_command();

        assert_eq!(command.get_program(), OsStr::new("opencode.exe"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("run"), OsStr::new("say hello && remove nothing")]
        );
    }

    #[test]
    fn can_request_json_output_for_programmatic_runs() {
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new("opencode.exe", prompt)
            .with_output_format(OpencodeOutputFormat::Json);
        let command = spec.to_command();

        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("run"),
                OsStr::new("--format"),
                OsStr::new("json"),
                OsStr::new("summarize the transcript"),
            ]
        );
    }

    #[test]
    fn captures_stdout_stderr_and_exit_code_from_process() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(
            temp.path(),
            "printf 'stdout:%s|%s\\n' \"$1\" \"$2\"; printf 'stderr:%s\\n' \"$1\" 1>&2",
        );
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt);

        let result = run_opencode(&spec).expect("fake executable should run");

        assert_eq!(result.stdout, "stdout:run|summarize the transcript\n");
        assert_eq!(result.stderr, "stderr:run\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(result.succeeded());
    }

    #[test]
    fn preserves_non_zero_exit_codes() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), "printf 'bad prompt' 1>&2; exit 7");
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt);

        let result = run_opencode(&spec).expect("fake executable should run");

        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr, "bad prompt");
        assert_eq!(result.exit_code, Some(7));
        assert!(!result.succeeded());
    }

    #[test]
    fn parses_minimal_json_events_and_ignores_other_event_types() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(
            temp.path(),
            "printf '%s\n' '{\"type\":\"text\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"text\":\"Hello from OpenCode\"}}' '{\"type\":\"tool_use\",\"timestamp\":2,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"title\":\"Shows working tree status\",\"output\":\"On branch main\"}}}' '{\"type\":\"step_start\",\"timestamp\":3,\"sessionID\":\"ses_1\",\"part\":{}}' '{\"type\":\"error\",\"timestamp\":4,\"sessionID\":\"ses_1\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"Provider failed\"}}}'",
        );
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec).expect("fake json executable should run");

        assert_eq!(
            result.events,
            vec![
                OpencodeJsonEvent::Text {
                    text: "Hello from OpenCode".to_string(),
                },
                OpencodeJsonEvent::ToolUse {
                    tool: "bash".to_string(),
                    status: OpencodeToolUseStatus::Completed,
                    detail: "Shows working tree status".to_string(),
                },
                OpencodeJsonEvent::Error {
                    name: "APIError".to_string(),
                    message: "Provider failed".to_string(),
                },
            ]
        );
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn reports_invalid_json_lines() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), "printf '%s\n' 'not json at all'");
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec);

        assert!(matches!(
            result,
            Err(OpencodeJsonRunError::InvalidJsonLine { line_number: 1, .. })
        ));
    }

    #[test]
    fn parses_tool_use_error_events() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(
            temp.path(),
            "printf '%s\n' '{\"type\":\"tool_use\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"command failed\"}}}'",
        );
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec).expect("fake json executable should run");

        assert_eq!(
            result.events,
            vec![OpencodeJsonEvent::ToolUse {
                tool: "bash".to_string(),
                status: OpencodeToolUseStatus::Error,
                detail: "command failed".to_string(),
            }]
        );
    }

    fn create_fake_opencode(directory: &Path, body: &str) -> PathBuf {
        let executable = directory.join("fake-opencode.sh");
        let script = format!("#!/bin/sh\n{body}\n");

        fs::write(&executable, script).expect("fake executable should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let permissions = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&executable, permissions)
                .expect("fake executable should be marked executable");
        }

        executable
    }
}
