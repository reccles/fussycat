//! The command-line adapter: argv and stdin in; the answer on stdout,
//! diagnostics on stderr, and an exit code out.

use std::fs::OpenOptions;
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use fussycat_core::agent::{Agent, Budget, DeclarationError, Task};
use fussycat_core::clock::Clock;
use fussycat_core::provider::Reasoning;
use fussycat_core::quilt::{BlockDecl, BlockName, Kind, Source, Text};
use fussycat_core::run::{Outcome, execute};
use fussycat_core::toolbox::{Tool, Toolbox};

use crate::reporters::{JsonlReporter, Reporters, StderrReporter, Verbosity, warn};
use crate::system::{
    CommandSource, FileSource, Limits, ShellTool, ShellToolError, SystemClock, ToolFile,
    ToolFileError, read_key,
};
#[cfg(unix)]
use crate::system::{Signal, on_stop_signal};
use fussycat_providers::{Adapter, ModelRef, ProviderName, RetryPolicy};

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_EXHAUSTED: u8 = 3;

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
struct Args {
    /// A stable block read from FILE, once (repeatable, order kept). They
    /// follow fussycat's core prompt, `(core)`, which every run starts with.
    /// Process substitution works: --stable <(cmd)
    #[arg(long = "stable", value_name = "FILE")]
    stable: Vec<PathBuf>,

    /// A live block: COMMAND's output, re-run before every turn and shown
    /// to the model after the log with its fetch time (repeatable).
    #[arg(long = "live", value_name = "COMMAND")]
    live: Vec<String>,

    /// Grant a tool the model may call (repeatable; takes several, so
    /// --tool tools/*.toml works). NAME=COMMAND grants a shell command that
    /// takes no arguments; a FILE ending in .toml declares a tool with its
    /// description and inputs (see examples/tools/).
    #[arg(long = "tool", value_name = "NAME=COMMAND|FILE", num_args = 1..)]
    tools: Vec<ToolGrant>,

    /// The model, named with its provider: ollama:gpt-oss:20b,
    /// venice:claude-haiku-5-5. A paid provider's key comes from a command
    /// named in `FUSSYCAT_<PROVIDER>_KEY_COMMAND` (a password manager's CLI), read
    /// through a pipe: the key is never in argv, the environment or a file.
    #[arg(
        long,
        env = "FUSSYCAT_MODEL",
        value_name = "PROVIDER:MODEL",
        required_unless_present = "list_models"
    )]
    model: Option<ModelRef>,

    /// List PROVIDER's models instead of running, one per line: the
    /// --model value, then context=, tools= and prices ($ per million
    /// tokens) where the provider reports them. Calls no model.
    #[arg(
        long,
        value_name = "PROVIDER",
        conflicts_with_all = ["stable", "live", "tools", "metrics"]
    )]
    list_models: Option<ProviderName>,

    /// The Ollama server.
    #[arg(
        long,
        env = "FUSSYCAT_OLLAMA_URL",
        default_value = "http://localhost:11434"
    )]
    ollama_url: String,

    /// Retry a failed model call this many times (429s, 5xx, dropped
    /// connections; never a rejected request), backing off 1s, 2s, 4s...
    /// 0 fails fast.
    #[arg(long, value_name = "N", default_value_t = RetryPolicy::DEFAULT_RETRIES)]
    retries: u32,

    /// Stop after this many model turns.
    #[arg(long, default_value_t = Budget::DEFAULT_MAX_TURNS)]
    max_turns: u32,

    /// Stop once the provider-reported cost of the run reaches USD (exit 3).
    /// The turn that crosses it is the last; an answer is always kept.
    /// Needs a provider that reports cost (venice, not ollama).
    #[arg(long, value_name = "USD")]
    max_cost: Option<f64>,

    /// Cut each tool result, and each live block, to BYTES, with a note
    /// telling the model so. Only that much of a command's output is kept.
    #[arg(long, value_name = "BYTES", default_value_t = Budget::DEFAULT_MAX_TOOL_OUTPUT)]
    max_tool_output: usize,

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
    command_timeout: f64,

    /// Give up on a model call that has sent nothing for SECS (fractions
    /// work), and retry it like a dropped connection. Default: 180 for
    /// anthropic and venice, which answer in seconds, and 600 for ollama,
    /// where a slow machine may take minutes over a long reply.
    #[arg(long, value_name = "SECS", value_parser = seconds)]
    provider_timeout: Option<f64>,

    /// Ask for replies of at most N tokens. A reply cut off at the limit
    /// fails the run (exit 1) rather than pass as an answer. Unset: the
    /// provider's default (ollama, venice), or the model's output limit up
    /// to 64000 (anthropic, which needs one).
    #[arg(long, value_name = "N")]
    max_output_tokens: Option<u64>,

    /// How hard a thinking model reasons before it answers: off, minimal,
    /// low, medium, high, xhigh or max. Each provider takes its own subset
    /// per model; one it doesn't take fails the run before anything is sent
    /// (exit 1). Unset: the provider's default, which varies by model.
    #[arg(long, value_name = "LEVEL")]
    reasoning_effort: Option<Reasoning>,

    /// Append a JSON line per turn, and one for the run, to FILE.
    #[arg(long, value_name = "FILE")]
    metrics: Option<PathBuf>,

    /// Report every turn on stderr, not just the run summary.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// No run summary on stderr (errors still show).
    #[arg(short, long)]
    quiet: bool,
}

pub async fn main() -> ExitCode {
    let args = Args::parse();
    #[cfg(unix)]
    if let Err(error) = on_stop_signal(stopped) {
        warn(format_args!("cannot watch for signals: {error}"));
        return ExitCode::from(EXIT_FAILED);
    }
    if let Some(provider) = args.list_models {
        return list_models(provider, &args.ollama_url).await;
    }
    let stdin = std::io::stdin();
    let piped = (!stdin.is_terminal()).then_some(stdin);
    match prepare(&args, piped) {
        Ok((agent, task, provider, mut reporters)) => {
            report(execute(&agent, &task, &provider, &SystemClock, &mut reporters).await)
        }
        Err(refused) => {
            warn(refused.message());
            ExitCode::from(refused.code())
        }
    }
}

/// A stop signal ends the run at once, whether a command is running (its
/// group is killed first) or not: it never goes on to another model call.
/// There's no answer, so nothing goes to stdout. The status is 128 + the
/// signal's number, as a shell reports a process the signal killed.
#[cfg(unix)]
fn stopped(signal: Signal) -> std::convert::Infallible {
    warn(format_args!("interrupted ({})", signal.name()));
    std::process::exit(128 + signal.number())
}

/// One `--tool` value: a shell command, or a tool file to load.
#[derive(Debug, Clone)]
enum ToolGrant {
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

/// Why a run never started. The line is *what was typed* against *what the
/// world did* (domain spec), and every site picks its side: there's no
/// conversion from a bare string, so no default to fall into.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    /// The flags, the environment's settings or the task are wrong (exit 2).
    /// Running it again won't help; fixing the command line will.
    Usage(String),
    /// The world failed a well-formed run (exit 1), like `cat /nope`: an
    /// unreadable file, a key command that fails.
    Failed(String),
}

impl Refused {
    fn code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Failed(_) => EXIT_FAILED,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Usage(message) | Self::Failed(message) => message,
        }
    }
}

/// A bad declaration is always what was typed.
impl From<DeclarationError> for Refused {
    fn from(error: DeclarationError) -> Self {
        Self::Usage(error.to_string())
    }
}

/// A wrong tool file is a usage error; one the world failed to provide is a
/// failure ([`ToolFileError::is_usage`]).
impl From<ToolFileError> for Refused {
    fn from(error: ToolFileError) -> Self {
        if error.is_usage() {
            Self::Usage(error.to_string())
        } else {
            Self::Failed(error.to_string())
        }
    }
}

/// A run, ready to execute.
type Prepared = (Agent, Task, Adapter, Reporters);

/// Everything that can be wrong before any model call, cheapest first: the
/// arguments, then the task on `stdin` (`None` when it's a terminal), and
/// only then what has side effects: tool files' help commands, the key
/// command (perhaps a Touch ID prompt) and creating `--metrics`. So a run
/// refused for a typo or an empty task touches nothing.
fn prepare(args: &Args, stdin: Option<impl Read>) -> Result<Prepared, Refused> {
    let model = args
        .model
        .as_ref()
        .ok_or_else(|| Refused::Usage("--model is required".into()))?;
    if args.max_cost.is_some() && !model.provider.reports_cost() {
        return Err(Refused::Usage(format!(
            "--max-cost needs a provider that reports cost, and `{model}` doesn't"
        )));
    }
    let (blocks, budget) = declarations(args)?;
    let task = read_task(stdin)?;
    // Side effects from here on.
    let tools = tools(&args.tools, limits(args))?;
    let agent = Agent::new(blocks, Toolbox::grant(tools)?, budget)?;
    let provider = model
        .connect(
            &args.ollama_url,
            api_key(model.provider, model)?,
            RetryPolicy {
                max_retries: args.retries,
            },
            args.provider_timeout.map(Duration::from_secs_f64),
        )
        .map_err(Refused::Usage)?; // only ever a missing key
    let reporters = reporters(args, model)?;
    Ok((agent, task, provider, reporters))
}

/// The provider's API key, from the command its variable names; `None` for
/// a provider that needs none. `who` names what wants it, in errors. No
/// variable is a usage error; a command that fails is the world's.
fn api_key(provider: ProviderName, who: &dyn std::fmt::Display) -> Result<Option<String>, Refused> {
    let Some(env) = provider.key_command_env() else {
        return Ok(None);
    };
    let command = std::env::var(env).map_err(|_| Refused::Usage(provider.needs_a_key(who)))?;
    read_key(env, &command).map(Some).map_err(Refused::Failed)
}

fn reporters(args: &Args, model: &ModelRef) -> Result<Reporters, Refused> {
    let verbosity = match (args.verbose, args.quiet) {
        (true, _) => Verbosity::Verbose,
        (_, true) => Verbosity::Quiet,
        _ => Verbosity::Summary,
    };
    let mut reporters = Reporters::default();
    reporters.push(StderrReporter::new(verbosity, std::io::stderr()));
    if let Some(path) = &args.metrics {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| Refused::Failed(format!("--metrics {}: {error}", path.display())))?;
        reporters.push(JsonlReporter::new(
            file,
            SystemClock.now(),
            &model.to_string(),
        ));
    }
    Ok(reporters)
}

/// The granted tools, loading tool files (and running their help commands).
fn tools(grants: &[ToolGrant], limits: Limits) -> Result<Vec<Box<dyn Tool>>, ToolFileError> {
    grants
        .iter()
        .map(|grant| match grant {
            ToolGrant::Shell(tool) => Ok(Box::new(tool.clone().within(limits)) as Box<dyn Tool>),
            ToolGrant::File(path) => Ok(Box::new(ToolFile::load(path, limits)?) as Box<dyn Tool>),
        })
        .collect()
}

/// The limits on every command a run starts: tools, live commands, help.
fn limits(args: &Args) -> Limits {
    Limits {
        timeout: Duration::from_secs_f64(args.command_timeout),
        keep: args.max_tool_output,
    }
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

/// The core prompt's block name, in reports and metrics like `(tools)`.
pub const CORE_BLOCK: &str = "(core)";

/// What the agent was given, which decides what the core prompt explains.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mechanisms {
    pub tools: bool,
    pub live: bool,
}

/// How fussycat runs, for every agent: the first stable block after the tools
/// (D6 in context/domain-design.spec.md). It's the CLI's because it talks
/// about stdout. It explains only what the agent has: told about live blocks
/// or tools it doesn't have, a model invents calls to them.
#[must_use]
pub fn core_prompt(has: Mechanisms) -> String {
    const GENERAL: &str = include_str!("core.md");
    const LIVE: &str = include_str!("core-live.md");
    const TOOLS: &str = include_str!("core-tools.md");
    let mut prompt = GENERAL.to_owned();
    if has.live {
        prompt.push_str(LIVE);
    }
    if has.tools {
        prompt.push_str(TOOLS);
    }
    prompt
}

/// The agent's blocks and budget: what's declared on the command line and
/// can be checked from it alone, before anything runs. (Duplicate blocks
/// and tools are caught by [`Agent::new`], once the tool files are loaded.)
fn declarations(args: &Args) -> Result<(Vec<BlockDecl>, Budget), DeclarationError> {
    let core = declare(
        Kind::Stable,
        CORE_BLOCK.into(),
        Text(core_prompt(Mechanisms {
            tools: !args.tools.is_empty(), // one tool per grant
            live: !args.live.is_empty(),
        })),
    );
    let stable = args.stable.iter().map(|path| {
        let path = tidy(path);
        declare(
            Kind::Stable,
            path.display().to_string(),
            FileSource::new(path),
        )
    });
    let live = args.live.iter().map(|command| {
        let source = CommandSource::new(command, limits(args));
        declare(Kind::Live, command.clone(), source)
    });
    let blocks = std::iter::once(core)
        .chain(stable)
        .chain(live)
        .collect::<Result<_, _>>()?;
    let mut budget = Budget::new(args.max_turns)?.with_max_tool_output(args.max_tool_output)?;
    if let Some(usd) = args.max_cost {
        budget = budget.with_max_cost(usd)?;
    }
    if let Some(tokens) = args.max_output_tokens {
        budget = budget.with_max_output_tokens(tokens)?;
    }
    if let Some(level) = args.reasoning_effort {
        budget = budget.with_reasoning(level);
    }
    Ok((blocks, budget))
}

/// `path` without `.` steps or doubled slashes, so `./a.md` and `a.md`
/// name one block (and a duplicate is caught). Lexical only: symlinks and
/// `..` are left as written, because resolving them needs the filesystem.
fn tidy(path: &Path) -> PathBuf {
    let tidied: PathBuf = path
        .components()
        .filter(|part| part != &Component::CurDir)
        .collect();
    if tidied.as_os_str().is_empty() {
        path.to_owned()
    } else {
        tidied
    }
}

fn declare(
    kind: Kind,
    name: String,
    source: impl Source + 'static,
) -> Result<BlockDecl, DeclarationError> {
    Ok(BlockDecl {
        name: BlockName::new(name)?,
        kind,
        source: Box::new(source),
    })
}

/// The task, from `stdin` (`None` when it's a terminal). No task, an empty
/// one, or one that isn't text is a usage error; a read that fails is the
/// world's.
fn read_task(stdin: Option<impl Read>) -> Result<Task, Refused> {
    let mut stdin = stdin.ok_or_else(|| Refused::Usage("no task: pipe one in on stdin".into()))?;
    let mut text = String::new();
    stdin.read_to_string(&mut text).map_err(|error| {
        if error.kind() == ErrorKind::InvalidData {
            Refused::Usage("the task on stdin isn't UTF-8 text".into())
        } else {
            Refused::Failed(format!("cannot read the task from stdin: {error}"))
        }
    })?;
    Ok(Task::new(text)?)
}

/// One line per model on stdout.
async fn list_models(provider: ProviderName, ollama_url: &str) -> ExitCode {
    // The provider asks for the key only if listing needs one, and hands
    // back its message alone: keep the refusal, which knows its exit code.
    let mut key_refused = None;
    let key = || {
        api_key(provider, &provider).map_err(|refused| {
            let message = refused.message().to_owned();
            key_refused = Some(refused);
            message
        })
    };
    match provider.list_models(ollama_url, key).await {
        Ok(models) => {
            let lines: String = models
                .iter()
                .map(|model| model.to_string() + "\n")
                .collect();
            print(&lines, ExitCode::SUCCESS)
        }
        Err(error) => {
            warn(format_args!("cannot list models: {error}"));
            ExitCode::from(key_refused.map_or(EXIT_FAILED, |refused| refused.code()))
        }
    }
}

fn report(outcome: Outcome) -> ExitCode {
    let code = ExitCode::from(exit_code(&outcome));
    match outcome {
        Outcome::Answered(mut answer) => {
            if !answer.ends_with('\n') {
                answer.push('\n');
            }
            return print(&answer, code);
        }
        Outcome::Exhausted(limit) => warn(format_args!("stopped: {limit}")),
        Outcome::Failed(failure) => warn(failure),
    }
    code
}

/// `text` on stdout, then `code`; or exit 1, saying why on stderr, if
/// stdout failed (a full disk). Never a panic: stdout is the contract.
fn print(text: &str, code: ExitCode) -> ExitCode {
    match emit(&mut std::io::stdout().lock(), text) {
        Ok(()) => code,
        Err(error) => {
            warn(format_args!("cannot write to stdout: {error}"));
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// Write all of `text` to `out` and flush it. A reader that closes the pipe
/// early (`| head -1`) is `Ok`: it took what it wanted, and every byte it
/// read was right, so the run's own exit code stands (see the README).
fn emit(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        written => written,
    }
}

fn exit_code(outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::Answered(_) => 0,
        Outcome::Failed(_) => EXIT_FAILED,
        Outcome::Exhausted(_) => EXIT_EXHAUSTED,
    }
}

#[cfg(test)]
mod tests;
