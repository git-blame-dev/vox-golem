use std::path::{Path, PathBuf};
use std::process::Command;

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
    args: Vec<String>,
}

impl OpencodeCommandSpec {
    pub fn new(executable_path: impl Into<PathBuf>, prompt: OpencodePrompt) -> Self {
        Self {
            executable_path: executable_path.into(),
            args: vec![prompt.text().to_string()],
        }
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.executable_path);
        command.args(&self.args);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::{OpencodeCommandSpec, OpencodePrompt, OpencodePromptError};
    use std::ffi::OsStr;
    use std::path::Path;

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
    fn builds_direct_argument_command_spec() {
        let prompt = OpencodePrompt::new("open the release checklist")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new("C:/Program Files/OpenCode/opencode.exe", prompt);

        assert_eq!(
            spec.executable_path(),
            Path::new("C:/Program Files/OpenCode/opencode.exe")
        );
        assert_eq!(spec.args(), &[String::from("open the release checklist")]);
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
            vec![OsStr::new("say hello && remove nothing")]
        );
    }
}
