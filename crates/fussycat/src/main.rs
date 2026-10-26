//! fussycat: a unix-philosophy agent CLI on the Agent Quilt pattern.
//! See `context/INTENT.md`.

mod cli;
mod providers;
mod reporters;
mod system;

/// The program's name: the prefix on every stderr line, `--help`'s name,
/// the Venice cache key's prefix. From Cargo, so a rename is one line.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cli::main().await
}
