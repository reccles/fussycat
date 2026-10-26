use std::str::FromStr;

use fussycat_core::quilt::{Source, SourceError};
use fussycat_core::toolbox::{Tool, ToolSpec};

use super::is_tool_name;
use super::process::{Ended, Limits, capture, isolated, output_of};

/// Run the author's `command` with `sh -c`, within `limits`. With
/// [`read_key`], the only place fussycat starts a shell (architecture eval
/// S9), and never with model text.
fn run(command: &str, limits: Limits) -> Result<String, String> {
    let mut shell = isolated("sh", &[]);
    shell.arg("-c").arg(command);
    output_of(shell, command, limits)
}

/// The most of a key command's output that's kept: far more than any key.
const KEY_LIMIT: usize = 64 * 1024;

/// An API key: the trimmed stdout of the author's key `command` (say
/// `pass show venice` or `op read op://Vault/venice/credential`), read
/// through a pipe. So the key is
/// never in fussycat's argv, environment or a file, only in its memory.
/// Its output is the secret, so no error ever shows it.
///
/// Nor the command: someone will paste the key itself into the variable.
/// Errors name the `variable` the command came from instead, and leave out
/// the shell's own complaints, which quote the command back.
///
/// It has no timeout: it runs once, before anything is sent or spent, and
/// may be waiting on a person (Touch ID, a pinentry prompt), so it stays in
/// fussycat's process group, where it can reach the terminal. A cron job's
/// key command shouldn't need a person; `timeout(1)` bounds the whole run.
pub fn read_key(variable: &str, command: &str) -> Result<String, String> {
    let mut shell = isolated("sh", &[]);
    shell.arg("-c").arg(command);
    let output = capture(shell, KEY_LIMIT, None)
        .map_err(|error| format!("{variable}: could not run sh: {error}"))?;
    match output.ended {
        Ended::Exited(status) if status.success() => {}
        Ended::Exited(status) => {
            return Err(key_command_failed(variable, status, &output.stderr.kept));
        }
        Ended::TimedOut => return Err(format!("{variable} timed out")), // it has none
    }
    if output.stdout.dropped() > 0 {
        return Err(format!(
            "{variable} printed more than {} KiB: that isn't a key",
            KEY_LIMIT / 1024
        ));
    }
    let key = String::from_utf8(output.stdout.kept)
        .map_err(|_| format!("{variable} printed a key that isn't UTF-8"))?;
    match key.trim() {
        "" => Err(format!("{variable} printed no key")),
        key => Ok(key.to_owned()),
    }
}

/// Why the key command failed, without its text. The shell's own lines
/// (`sh: sk-...: command not found`, `sh: -c: line 0: syntax error near
/// unexpected token ...`) quote the command, so they're dropped; the
/// program's own (`op: not signed in`) are the useful part and stay. Exit
/// 126 or 127 means sh couldn't run the command at all: that gets a hint.
fn key_command_failed(variable: &str, status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let code = status.code();
    let status = code.map_or_else(|| status.to_string(), |code| format!("exit {code}"));
    if matches!(code, Some(126 | 127)) {
        return format!(
            "{variable} failed ({status}): it must hold a command that prints the key \
             (e.g. a password manager's CLI), not the key itself; \
             if it does, check the program is installed and on PATH"
        );
    }
    let stderr = String::from_utf8_lossy(stderr);
    let said: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.starts_with("sh:"))
        .collect();
    match said.join("\n").trim() {
        "" => format!("{variable} failed ({status})"),
        said => format!("{variable} failed ({status}): {said}"),
    }
}

/// A live block's content from a shell command (`--live 'cmd'`), re-run
/// every turn. One that exits non-zero or times out fails the fetch.
pub struct CommandSource {
    command: String,
    limits: Limits,
}

impl CommandSource {
    pub fn new(command: impl Into<String>, limits: Limits) -> Self {
        Self {
            command: command.into(),
            limits,
        }
    }
}

impl Source for CommandSource {
    fn fetch(&self) -> Result<String, SourceError> {
        run(&self.command, self.limits).map_err(SourceError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellToolError {
    #[error("expected NAME=COMMAND or a tool file ending in .toml, got `{0}`")]
    Malformed(String),
    #[error("tool name `{0}` must be 1-64 characters of a-z, A-Z, 0-9, `_` or `-`")]
    BadName(String),
}

/// A shell command granted as a tool with `--tool 'name=cmd'`.
///
/// The command is the author's, written on the command line, and runs
/// exactly as given. The model can only trigger it: the tool takes no
/// arguments, and anything the model sends is ignored, so model output
/// never reaches a shell.
#[derive(Debug, Clone)]
pub struct ShellTool {
    spec: ToolSpec,
    command: String,
    limits: Limits,
}

impl ShellTool {
    /// This tool, run within `limits` (parsed, it has the defaults).
    #[must_use]
    pub fn within(self, limits: Limits) -> Self {
        Self { limits, ..self }
    }
}

impl FromStr for ShellTool {
    type Err = ShellToolError;

    fn from_str(grant: &str) -> Result<Self, Self::Err> {
        let (name, command) = grant
            .split_once('=')
            .filter(|(_, command)| !command.trim().is_empty())
            .ok_or_else(|| ShellToolError::Malformed(grant.to_owned()))?;
        if !is_tool_name(name) {
            return Err(ShellToolError::BadName(name.to_owned()));
        }
        Ok(Self {
            spec: ToolSpec {
                name: name.to_owned(),
                description: format!(
                    "Runs `{command}` and returns its output. Takes no arguments."
                ),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            command: command.to_owned(),
            limits: Limits::DEFAULT,
        })
    }
}

impl Tool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(&self, _arguments: &serde_json::Value) -> Result<String, String> {
        run(&self.command, self.limits)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use serde_json::json;

    fn tool(grant: &str) -> ShellTool {
        grant.parse().unwrap()
    }

    #[test]
    fn parses_name_and_command() {
        let now = tool("now=date -u");
        assert_eq!(now.spec().name, "now");
        assert_eq!(now.command, "date -u");
        assert_eq!(tool("eq=a=b").command, "a=b", "only the first `=` splits");
    }

    #[test]
    fn rejects_malformed_grants() {
        assert_eq!(
            "now".parse::<ShellTool>().unwrap_err(),
            ShellToolError::Malformed("now".into())
        );
        assert_eq!(
            "now= ".parse::<ShellTool>().unwrap_err(),
            ShellToolError::Malformed("now= ".into())
        );
        assert_eq!(
            "=date".parse::<ShellTool>().unwrap_err(),
            ShellToolError::BadName(String::new())
        );
        assert_eq!(
            "my tool=date".parse::<ShellTool>().unwrap_err(),
            ShellToolError::BadName("my tool".into())
        );
    }

    #[test]
    fn shell_tools_and_live_commands_get_a_clean_environment() {
        // Cargo sets this for every test: a variable fussycat has, standing in
        // for a credential. Nothing can pass it to a `NAME=CMD` tool.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let peek = "echo ${CARGO_MANIFEST_DIR:-unset}";
        assert_eq!(
            tool(&format!("e={peek}")).invoke(&json!({})),
            Ok("unset\n".into())
        );
        assert_eq!(source(peek).fetch(), Ok("unset\n".into()));
    }

    #[test]
    fn runs_the_command() {
        assert_eq!(
            tool("hi=echo hello").invoke(&json!({})),
            Ok("hello\n".into())
        );
    }

    #[test]
    fn a_failing_command_is_an_err_result_with_its_stderr() {
        let error = tool("bad=echo oops >&2; exit 3")
            .invoke(&json!({}))
            .unwrap_err();
        assert!(error.contains("oops"), "{error}");
    }

    #[test]
    fn a_command_source_fetches_its_output_and_reports_failure() {
        assert_eq!(source("echo tick").fetch(), Ok("tick\n".into()));
        let error = source("exit 7").fetch().unwrap_err();
        assert!(error.0.contains("`exit 7` failed"), "{error}");
    }

    fn source(command: &str) -> CommandSource {
        CommandSource::new(command, Limits::DEFAULT)
    }

    const BRIEF: Limits = Limits {
        timeout: Duration::from_millis(200),
        ..Limits::DEFAULT
    };

    /// A hung tool is an `Err` result the model sees, and the run goes on.
    #[test]
    fn a_tool_past_its_timeout_is_an_err_result() {
        let error = tool("hang=sleep 30").within(BRIEF).invoke(&json!({}));
        assert_eq!(error, Err("`sleep 30` timed out after 0.2s: ".into()));
    }

    /// A hung live command fails the fetch, so the block renders failed
    /// (or stale), like any failing live command.
    #[test]
    fn a_live_command_past_its_timeout_fails_the_fetch() {
        let error = CommandSource::new("sleep 30", BRIEF).fetch().unwrap_err();
        assert!(error.0.contains("timed out after 0.2s"), "{error}");
    }

    /// `sh -c 'a & b'` forks `a`: killing only sh would leave it running,
    /// and holding the pipe. The timeout kills the whole process group.
    ///
    /// The job proves it survived by writing a file later. Not `kill -0`:
    /// a killed job is a zombie until its new parent reaps it, and a
    /// container whose PID 1 never reaps (`tail -f /dev/null`, as CI
    /// runners use) keeps it one, so `kill -0` says it's alive.
    #[test]
    fn a_timeout_kills_what_the_command_started() {
        let proof = std::env::temp_dir().join(format!("fussycat-survived-{}", std::process::id()));
        let _ = std::fs::remove_file(&proof);
        let script = format!("(sleep 0.6; touch '{}') & wait", proof.display());
        let started = Instant::now();
        let error = run(&script, BRIEF).unwrap_err();
        assert!(error.contains("timed out after 0.2s"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // Well past when a surviving job would have written.
        std::thread::sleep(Duration::from_millis(1200).saturating_sub(started.elapsed()));
        let survived = proof.exists();
        let _ = std::fs::remove_file(&proof);
        assert!(!survived, "the job it started survived the timeout");
    }

    /// The command is done, but a job it left behind holds stdout open:
    /// fussycat takes the command's output and moves on.
    #[test]
    fn a_job_left_holding_the_pipe_never_hangs_the_read() {
        let started = Instant::now();
        assert_eq!(run("sleep 5 & echo hi", Limits::DEFAULT), Ok("hi\n".into()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn live_output_past_the_cap_is_cut_visibly() {
        let limits = Limits {
            keep: 100,
            ..Limits::DEFAULT
        };
        let content = CommandSource::new("seq 100000", limits).fetch().unwrap();
        assert!(content.len() <= 100, "{content}");
        assert!(content.starts_with("1\n2\n"), "{content}");
        assert!(
            content.ends_with("of 588895 bytes]"),
            "the note gives the whole size: {content}"
        );
    }

    #[test]
    fn arch_model_text_never_reaches_a_shell() {
        let echo = tool("echo=echo fixed");
        let hostile = json!({"command": "; touch /tmp/pwned", "args": ["$(id)"]});
        assert_eq!(echo.invoke(&hostile), Ok("fixed\n".into()));
    }

    const VARIABLE: &str = "FUSSYCAT_TEST_KEY_COMMAND";

    #[test]
    fn a_key_is_the_trimmed_output_of_its_command() {
        assert_eq!(
            read_key(VARIABLE, "printf ' sk-test\\n'"),
            Ok("sk-test".into())
        );
    }

    /// The key command's stdout is the secret: a failure, or an empty
    /// answer, names the variable and shows its stderr, never what it
    /// printed.
    #[test]
    fn a_failing_key_command_never_shows_its_output() {
        let error = read_key(VARIABLE, "printf sk-%s leaked; echo locked >&2; exit 1").unwrap_err();
        assert_eq!(error, format!("{VARIABLE} failed (exit 1): locked"));
        assert_eq!(
            read_key(VARIABLE, "true"),
            Err(format!("{VARIABLE} printed no key"))
        );
    }

    #[test]
    fn a_key_command_that_prints_too_much_is_refused() {
        assert_eq!(
            read_key(VARIABLE, "yes | head -c 100000"),
            Err(format!(
                "{VARIABLE} printed more than 64 KiB: that isn't a key"
            ))
        );
    }

    /// The newcomer's mistake: the key itself in the variable. sh says
    /// `sh: sk-...: command not found` (127); none of that may get out.
    #[test]
    fn a_key_pasted_in_place_of_a_command_never_shows() {
        let key = "sk-ant-api03-FAKE0123456789";
        let error = read_key(VARIABLE, key).unwrap_err();
        assert!(!error.contains(key), "{error}");
        assert!(
            error.starts_with(&format!("{VARIABLE} failed (exit 127): ")),
            "{error}"
        );
        assert!(error.contains("a command that prints the key"), "{error}");
        // sh quotes all or part of a key it can't parse, or one it reads as
        // a redirection. The status is the shell's (bash says 1 for the
        // redirection, dash 2), so only the shape is pinned: the variable
        // and a status, and nothing sh said.
        for key in ["sk-FAKE(0123456789", "sk-FAKE<0123456789"] {
            let error = read_key(VARIABLE, key).unwrap_err();
            let status = error
                .strip_prefix(&format!("{VARIABLE} failed (exit "))
                .and_then(|rest| rest.strip_suffix(')'));
            assert!(
                status.is_some_and(|s| s.parse::<u8>().is_ok()),
                "{key}: {error}"
            );
        }
    }
}
