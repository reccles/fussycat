//! fussycat: a unix-philosophy agent CLI on the Agent Quilt pattern.
//! See `context/INTENT.md`.

mod cli;
#[cfg(test)]
mod paid;
mod reporters;
mod system;

/// The program's name: the prefix on every stderr line, `--help`'s name.
/// From Cargo, so a rename is one line. (The Venice cache key's prefix is
/// spelled out in `fussycat-providers`, so a rename doesn't reset caches.)
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    cli::main().await
}
