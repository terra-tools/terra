use std::path::PathBuf;

use crate::backend::tap::OutputTap;

const DEFAULT_SHELL: &str = "/bin/bash";

#[derive(Clone)]
pub struct BackendSettings {
    pub shell: String,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    /// terra patch: called with every chunk of bytes the child writes, on the
    /// PTY reader thread, before the parser sees them. `None` — the default —
    /// installs no tap and copies nothing. terra uses it for per-tab
    /// transcripts; see `tap.rs`.
    pub output_tap: Option<OutputTap>,
}

// Hand-written because a tap is a closure, which has no Debug.
impl std::fmt::Debug for BackendSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendSettings")
            .field("shell", &self.shell)
            .field("args", &self.args)
            .field("working_directory", &self.working_directory)
            .field("output_tap", &self.output_tap.is_some())
            .finish()
    }
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            shell: DEFAULT_SHELL.to_string(),
            args: vec![],
            working_directory: None,
            output_tap: None,
        }
    }
}
