//! Adapters to the local system: files, commands, tools, signals and the
//! clock.

mod clock;
mod file;
mod process;
mod shell;
#[cfg(unix)]
mod signals;
mod tool_file;

pub use clock::SystemClock;
pub use file::FileSource;
pub use process::Limits;
pub use shell::{CommandSource, ShellTool, ShellToolError, read_key};
#[cfg(unix)]
pub use signals::{Signal, on_stop_signal};
pub use tool_file::{ToolFile, ToolFileError};

/// What providers accept as a tool name: 1-64 of `a-z A-Z 0-9 _ -`.
fn is_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
