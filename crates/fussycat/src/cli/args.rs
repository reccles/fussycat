//! The command line: every flag, its help text and how its value parses.
//! What a run does with them is the parent module's.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use fussycat_core::agent::Budget;
use fussycat_core::provider::Reasoning;
use fussycat_providers::{ModelRef, ProviderName, RetryPolicy};

use crate::system::{Limits, ShellTool, ShellToolError};

/// The command line is the permission manifest: a bare run grants nothing,
/// and grants (`--tool`) come only from flags, never from the environment.
#[derive(Debug, Parser)]
#[command(
    version,
    about = "An agent that reads a task on stdin and writes the answer to stdout.",
    after_help = "Exit status: 0 answered; 1 failed (a provider, a file, a key command: \
                  the world); 2 usage error (a flag, a setting or the task: what you \
                  typed); 3 budget exhausted; 130, 143 stopped by SIGINT, SIGTERM \
                  (128 + the signal), with the running command killed. A reader \
                  that stops early (| head) doesn't change it."
)]
pub(super) struct Args {
    /// A stable block read from FILE, once (repeatable, order kept). They
    /// follow fussycat's core prompt, `(core)`, which every run starts with.
    /// Process substitution works: --stable <(cmd)
    #[arg(long = "stable", value_name = "FILE")]
    pub(super) stable: Vec<PathBuf>,

    /// A live block: COMMAND's output, re-run before every turn and shown
    /// to the model after the log with its fetch time (repeatable).
    /// mcp:SERVER.VIEW is instead a `[live.VIEW]` of an --mcp server's file.
    #[arg(long = "live", value_name = "COMMAND|mcp:SERVER.VIEW")]
    pub(super) live: Vec<String>,

    /// Grant a tool the model may call (repeatable; takes several, so
    /// --tool tools/*.toml works). NAME=COMMAND grants a shell command that
    /// takes no arguments; a FILE ending in .toml declares a tool with its
    /// description and inputs (see examples/tools/).
    #[arg(long = "tool", value_name = "NAME=COMMAND|FILE", num_args = 1..)]
    pub(super) tools: Vec<ToolGrant>,

    /// Grant an MCP server's tools (repeatable; takes several, so
    /// --mcp mcp/*.toml works). The FILE says how to run the server, which
    /// of its tools the model may call (as `SERVER__TOOL`) and its live views.
    /// See docs/mcp.md.
    #[arg(long = "mcp", value_name = "FILE", num_args = 1..)]
    pub(super) mcp: Vec<PathBuf>,

    /// Start the MCP server FILE declares and list the tools it offers,
    /// instead of running: one per line, as `tools = [...]` names it, with
    /// what the server marks it. Calls no model.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = ["stable", "live", "tools", "mcp", "metrics", "list_models"]
    )]
    pub(super) list_tools: Option<PathBuf>,

    /// The model, named with its provider: ollama:gpt-oss:20b,
    /// llamacpp:qwen3.8-27b, venice:claude-haiku-5-5. A paid provider's key comes from a command
    /// named in `FUSSYCAT_<PROVIDER>_KEY_COMMAND` (a password manager's CLI), read
    /// through a pipe: the key is never in argv, the environment or a file.
    #[arg(
        long,
        env = "FUSSYCAT_MODEL",
        value_name = "PROVIDER:MODEL",
        required_unless_present_any = ["list_models", "list_tools"]
    )]
    pub(super) model: Option<ModelRef>,

    /// List PROVIDER's models instead of running, one per line: the
    /// --model value, then context=, tools= and prices ($ per million
    /// tokens) where the provider reports them. Calls no model.
    #[arg(
        long,
        value_name = "PROVIDER",
        conflicts_with_all = ["stable", "live", "tools", "metrics"]
    )]
    pub(super) list_models: Option<ProviderName>,

    /// The Ollama server.
    #[arg(
        long,
        env = "FUSSYCAT_OLLAMA_URL",
        default_value = "http://localhost:11434"
    )]
    pub(super) ollama_url: String,

    /// The llama.cpp server (`llama-server`), with its /v1. A server
    /// started with --api-key gets its key from the command in
    /// `FUSSYCAT_LLAMACPP_KEY_COMMAND`; one without needs nothing.
    #[arg(
        long,
        env = "FUSSYCAT_LLAMACPP_URL",
        default_value = "http://localhost:8080/v1"
    )]
    pub(super) llamacpp_url: String,

    /// Retry a failed model call this many times (429s, 5xx, dropped
    /// connections; never a rejected request), backing off 1s, 2s, 4s...
    /// 0 fails fast.
    #[arg(long, value_name = "N", default_value_t = RetryPolicy::DEFAULT_RETRIES)]
    pub(super) retries: u32,

    /// Stop after this many model turns.
    #[arg(long, default_value_t = Budget::DEFAULT_MAX_TURNS)]
    pub(super) max_turns: u32,

    /// Stop once the provider-reported cost of the run reaches USD (exit 3).
    /// The turn that crosses it is the last; an answer is always kept.
    /// Needs a provider that reports cost (venice, not ollama).
    #[arg(long, value_name = "USD")]
    pub(super) max_cost: Option<f64>,

    /// Cut each tool result, and each live block, to BYTES, with a note
    /// telling the model so. Only that much of a command's output is kept.
    #[arg(long, value_name = "BYTES", default_value_t = Budget::DEFAULT_MAX_TOOL_OUTPUT)]
    pub(super) max_tool_output: usize,

    /// Kill a command still running after SECS (fractions work): a tool's
    /// result is then an error the model sees, a live block shows failed or
    /// stale, and a tool file's help fails the run. Applies to every command
    /// but the key command, which may be waiting on you.
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = Limits::DEFAULT_TIMEOUT.as_secs_f64(),
        value_parser = seconds
    )]
    pub(super) command_timeout: f64,

    /// Give up on a model call that has sent nothing for SECS (fractions
    /// work), and retry it like a dropped connection. Default: 180 for
    /// anthropic and venice, which answer in seconds, and 600 for ollama,
    /// where a slow machine may take minutes over a long reply.
    #[arg(long, value_name = "SECS", value_parser = seconds)]
    pub(super) provider_timeout: Option<f64>,

    /// Ask for replies of at most N tokens. A reply cut off at the limit
    /// fails the run (exit 1) rather than pass as an answer. Unset: the
    /// provider's default (ollama, venice), or the model's output limit up
    /// to 64000 (anthropic, which needs one).
    #[arg(long, value_name = "N")]
    pub(super) max_output_tokens: Option<u64>,

    /// How hard a thinking model reasons before it answers: off, minimal,
    /// low, medium, high, xhigh or max. Each provider takes its own subset
    /// per model; one it doesn't take fails the run before anything is sent
    /// (exit 1). Unset: the provider's default, which varies by model.
    #[arg(long, value_name = "LEVEL")]
    pub(super) reasoning_effort: Option<Reasoning>,

    /// Append a JSON line per turn, and one for the run, to FILE.
    #[arg(long, value_name = "FILE")]
    pub(super) metrics: Option<PathBuf>,

    /// Report every turn on stderr, not just the run summary.
    #[arg(short, long, conflicts_with = "quiet")]
    pub(super) verbose: bool,

    /// No run summary on stderr (errors still show).
    #[arg(short, long)]
    pub(super) quiet: bool,
}

/// One `--tool` value: a shell command, or a tool file to load.
#[derive(Debug, Clone)]
pub(super) enum ToolGrant {
    Shell(ShellTool),
    File(PathBuf),
}

impl FromStr for ToolGrant {
    type Err = ShellToolError;

    /// `NAME=COMMAND` when it splits into a valid name and a command, else a
    /// path ending in `.toml`.
    fn from_str(grant: &str) -> Result<Self, Self::Err> {
        match grant.parse::<ShellTool>() {
            Ok(tool) => Ok(Self::Shell(tool)),
            Err(_) if is_toml(grant) => Ok(Self::File(grant.into())),
            Err(error) => Err(error),
        }
    }
}

fn is_toml(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
}

/// `--command-timeout`, `--provider-timeout`: a positive number of seconds
/// that fits a Duration.
fn seconds(text: &str) -> Result<f64, String> {
    let secs: f64 = text
        .parse()
        .map_err(|_| format!("`{text}` isn't a number"))?;
    // Refuses NaN, negatives and the too long; zero passes it, not this.
    match Duration::try_from_secs_f64(secs) {
        Ok(duration) if !duration.is_zero() => Ok(secs),
        _ if secs > 1.0 => Err(format!("`{text}` is too long")),
        _ => Err(format!("`{text}` must be more than 0 seconds")),
    }
}
