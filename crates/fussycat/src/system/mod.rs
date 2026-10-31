//! Adapters to the local system: files, commands, tools, signals and the
//! clock.

mod clock;
mod file;
mod mcp;
mod process;
mod shell;
#[cfg(unix)]
mod signals;
mod tool_file;

pub use clock::SystemClock;
pub use file::FileSource;
pub use mcp::{McpError, McpServer, McpView};
pub use process::Limits;
pub use shell::{CommandSource, ShellTool, ShellToolError, read_key};
#[cfg(unix)]
pub use signals::{Signal, on_stop_signal};
pub use tool_file::{ToolFile, ToolFileError};

/// The tool-file and MCP-file format this fussycat reads. A file may say
/// `version = 1`; without it, it's read as this one. A file for a newer
/// format is refused rather than half-understood.
pub const FILE_VERSION: u32 = 1;

/// `version`, from a tool or MCP file, if this fussycat reads it.
fn check_version(version: Option<u32>) -> Result<(), String> {
    match version {
        None | Some(FILE_VERSION) => Ok(()),
        Some(newer) if newer > FILE_VERSION => Err(format!(
            "`version = {newer}` is newer than this fussycat reads ({FILE_VERSION}): upgrade fussycat"
        )),
        Some(other) => Err(format!(
            "`version = {other}` isn't a version (this fussycat reads {FILE_VERSION})"
        )),
    }
}

/// What providers accept as a tool name: 1-64 of `a-z A-Z 0-9 _ -`.
fn is_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Tools and sources run to the end, for tests: a runtime per call.
#[cfg(test)]
pub(crate) mod now {
    use fussycat_core::quilt::{Source, SourceError};
    use fussycat_core::toolbox::Tool;

    pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    pub(crate) trait InvokeNow {
        fn invoke_now(&self, arguments: &serde_json::Value) -> Result<String, String>;
    }

    impl<T: Tool + ?Sized> InvokeNow for T {
        fn invoke_now(&self, arguments: &serde_json::Value) -> Result<String, String> {
            block_on(self.invoke(arguments))
        }
    }

    pub(crate) trait FetchNow {
        fn fetch_now(&self) -> Result<String, SourceError>;
    }

    impl<T: Source + ?Sized> FetchNow for T {
        fn fetch_now(&self) -> Result<String, SourceError> {
            block_on(self.fetch())
        }
    }
}
