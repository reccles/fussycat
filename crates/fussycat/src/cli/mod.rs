//! The command-line adapter: argv and stdin in; the answer on stdout,
//! diagnostics on stderr, and an exit code out.

use std::fs::OpenOptions;
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use fussycat_core::agent::{Agent, Budget, DeclarationError, Task};
use fussycat_core::clock::Clock;
use fussycat_core::quilt::{BlockDecl, BlockName, Kind, Source, Text};
use fussycat_core::run::{Outcome, execute};
use fussycat_core::toolbox::{Tool, Toolbox};

mod args;
mod mcp;

use args::{Args, ToolGrant};

use crate::reporters::{JsonlReporter, Reporters, StderrReporter, Verbosity, run_id, warn};
use crate::system::{
    CommandSource, FileSource, Limits, SystemClock, ToolFile, ToolFileError,
    keep_inherited_fds_from_children, read_key,
};
#[cfg(unix)]
use crate::system::{Signal, on_stop_signal};
use fussycat_providers::{Adapter, ModelRef, ProviderName, RetryPolicy};

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_EXHAUSTED: u8 = 3;

pub async fn main() -> ExitCode {
    keep_inherited_fds_from_children();
    let args = Args::parse();
    #[cfg(unix)]
    if let Err(error) = on_stop_signal(stopped) {
        warn(format_args!("cannot watch for signals: {error}"));
        return ExitCode::from(EXIT_FAILED);
    }
    if let Some(provider) = args.list_models {
        return list_models(provider, &args.ollama_url).await;
    }
    if let Some(path) = &args.list_tools {
        return mcp::list_tools(path, limits(&args)).await;
    }
    let stdin = std::io::stdin();
    let piped = (!stdin.is_terminal()).then_some(stdin);
    match prepare(&args, piped).await {
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
/// only then what has side effects: tool files' help commands, starting MCP
/// servers, the key command (perhaps a Touch ID prompt) and creating
/// `--metrics`. So a run refused for a typo or an empty task touches
/// nothing.
async fn prepare(args: &Args, stdin: Option<impl Read>) -> Result<Prepared, Refused> {
    let model = args
        .model
        .as_ref()
        .ok_or_else(|| Refused::Usage("--model is required".into()))?;
    if args.max_cost.is_some() && !model.provider.reports_cost() {
        return Err(Refused::Usage(format!(
            "--max-cost needs a provider that reports cost, and `{model}` doesn't"
        )));
    }
    let servers = mcp::load(&args.mcp, limits(args))?;
    let (blocks, budget) = declarations(args, &servers)?;
    let task = read_task(stdin)?;
    // Side effects from here on.
    let mut tools = tools(&args.tools, limits(args))?;
    tools.extend(mcp::start(&servers).await?);
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
            run_id(),
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
/// in the `servers`' files, checked before anything runs. (Duplicate blocks
/// and tools are caught by [`Agent::new`], once the tools are known.)
fn declarations(args: &Args, servers: &mcp::Servers) -> Result<(Vec<BlockDecl>, Budget), Refused> {
    let core = declare(
        Kind::Stable,
        CORE_BLOCK.into(),
        Text(core_prompt(Mechanisms {
            // One tool per grant; a server may grant views alone.
            tools: !args.tools.is_empty() || servers.iter().any(|server| server.grants_tools()),
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
    let live = args.live.iter().map(|spelled| {
        let name = spelled.clone();
        if let Some(view) = mcp::view(spelled, servers) {
            return Ok(declare(Kind::Live, name, view?)?);
        }
        Ok(declare(
            Kind::Live,
            name,
            CommandSource::new(spelled, limits(args)),
        )?)
    });
    let blocks = std::iter::once(core.map_err(Refused::from))
        .chain(stable.map(|block| block.map_err(Refused::from)))
        .chain(live)
        .collect::<Result<_, Refused>>()?;
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
        Outcome::Answered(answer) => return print(&as_printed(&answer), code),
        Outcome::Exhausted(limit) => warn(format_args!("stopped: {limit}")),
        Outcome::Failed(failure) => warn(failure),
    }
    code
}

/// The answer as stdout gets it: from its first line with something on it
/// (a thinking model can leave blank lines after its reasoning, and a
/// script reads the first line), to exactly one trailing newline. The first
/// line keeps its indentation: it may be code.
fn as_printed(answer: &str) -> String {
    let first = answer.find(|c: char| !c.is_whitespace()).unwrap_or(0);
    let line = answer[..first].rfind('\n').map_or(0, |newline| newline + 1);
    let mut printed = answer[line..].to_owned();
    if !printed.ends_with('\n') {
        printed.push('\n');
    }
    printed
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
