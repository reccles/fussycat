//! Every child fussycat starts: built with a clean environment, then run with
//! a timeout and a bounded read, so no command can hang a run or fill its
//! memory.

use std::ffi::OsString;
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use fussycat_core::agent::Budget;
use fussycat_core::toolbox::cap_within;

/// The environment variables every child gets, when fussycat has them: what
/// programs need to run, and nothing that grants anything. Any other
/// variable, credentials above all, reaches a child only when a tool file
/// names it (`env = [...]`). Deny by default, like `sudo`'s `env_reset`.
pub const INHERITED: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ", "TMPDIR",
];

/// A command for `program` with no stdin and a clean environment: only
/// [`INHERITED`] and the `passed` names, taken from fussycat's own. Every
/// child fussycat starts is built here (architecture eval S10).
pub fn isolated(program: &str, passed: &[String]) -> Command {
    isolated_from(program, passed, |name| std::env::var_os(name))
}

fn isolated_from(
    program: &str,
    passed: &[String],
    parent: impl Fn(&str) -> Option<OsString>,
) -> Command {
    let mut command = Command::new(program);
    command.env_clear().stdin(Stdio::null());
    let names = INHERITED
        .iter()
        .copied()
        .chain(passed.iter().map(String::as_str));
    for name in names {
        if let Some(value) = parent(name) {
            command.env(name, value);
        }
    }
    command
}

/// How long a command may run, and how many bytes of each of its stdout
/// and stderr are kept: the limits on a tool, a `--live` command and a tool
/// file's help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    pub keep: usize,
}

impl Limits {
    /// A minute: room for a slow search or page fetch (the web-fetch tool
    /// gives up after 30 s itself, and says why), while a hung command
    /// costs an unattended run a minute, not the night.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
    pub const DEFAULT: Self = Self {
        timeout: Self::DEFAULT_TIMEOUT,
        keep: Budget::DEFAULT_MAX_TOOL_OUTPUT,
    };
}

/// Run `command` within `limits`: its stdout on success, or an error
/// carrying its status (or the timeout), stderr and stdout. `shown` names
/// it in messages. Output past the cap is read and counted, not kept, so
/// the cut note the model sees still says how much there was.
pub fn output_of(command: Command, shown: &str, limits: Limits) -> Result<String, String> {
    let Captured {
        ended,
        stdout,
        stderr,
    } = capture(command, limits.keep, Some(limits.timeout))
        .map_err(|error| format!("could not run `{shown}`: {error}"))?;
    let why = match ended {
        Ended::Exited(status) if status.success() => {
            return Ok(bounded(
                stdout.text(),
                stdout.dropped(),
                limits.keep,
                "output",
            ));
        }
        Ended::Exited(status) => format!("failed ({status})"),
        Ended::TimedOut => format!("timed out after {}", seconds(limits.timeout)),
    };
    let message = format!("`{shown}` {why}: {}{}", stderr.text(), stdout.text());
    let dropped = stderr.dropped() + stdout.dropped();
    Err(bounded(message, dropped, limits.keep, "error"))
}

/// `text`, which lost `dropped` bytes to a bounded read, cut to fit `keep`
/// with a note saying how much there was. Untouched if nothing was dropped.
pub fn bounded(text: String, dropped: usize, keep: usize, what: &str) -> String {
    if dropped == 0 {
        return text;
    }
    let total = text.len() + dropped;
    cap_within(text, total, keep, what)
}

/// `60s`, `0.5s`: a timeout as it's given on the command line.
pub fn seconds(duration: Duration) -> String {
    format!("{}s", duration.as_secs_f64())
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited(ExitStatus),
    /// Still running at the timeout, so it was killed.
    TimedOut,
}

/// One output stream: the first bytes, and how many there were in all.
#[derive(Debug, Default)]
pub struct Stream {
    pub kept: Vec<u8>,
    pub total: usize,
}

impl Stream {
    fn push(&mut self, chunk: &[u8], keep: usize) {
        let room = keep.saturating_sub(self.kept.len()).min(chunk.len());
        self.kept.extend_from_slice(&chunk[..room]);
        self.total += chunk.len();
    }

    /// Bytes read but not kept.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.total - self.kept.len()
    }

    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.kept).into_owned()
    }
}

#[derive(Debug)]
pub struct Captured {
    pub ended: Ended,
    pub stdout: Stream,
    pub stderr: Stream,
}

/// How long the readers get once the command is gone. Its output is in the
/// pipes by then, but a grandchild (`sh -c 'job & echo hi'`) can hold them
/// open for as long as it runs, and what it prints later isn't the
/// command's.
const DRAIN_GRACE: Duration = Duration::from_millis(200);
/// How often a command with a timeout is checked on.
const POLL: Duration = Duration::from_millis(5);

/// Run `command`, keeping the first `keep` bytes of each of its stdout and
/// stderr. Both are read on their own threads while it runs, so neither
/// pipe can fill and stall it; past `keep` they're read and counted, not
/// kept. With a `timeout`, the command runs in its own [`Group`], and at
/// the timeout the whole group is killed: `sh -c` and anything it started.
/// Without one (only the key command, which may be waiting on a person), it
/// stays in fussycat's group, where it can reach the terminal.
pub fn capture(
    mut command: Command,
    keep: usize,
    timeout: Option<Duration>,
) -> io::Result<Captured> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Held until the readers are done: a job the command left holding its
    // pipes is in its group, and [`stop_all`] should get that too.
    let (mut child, _group) = if timeout.is_some() {
        Group::spawn(command).map(|(child, group)| (child, Some(group)))?
    } else {
        (command.spawn()?, None)
    };
    let (done, finished) = channel();
    let stdout = read(child.stdout.take(), keep, done.clone())?;
    let stderr = read(child.stderr.take(), keep, done)?;
    let ended = wait(&mut child, timeout)?;
    let grace = Instant::now() + DRAIN_GRACE;
    for _ in 0..2 {
        let left = grace.saturating_duration_since(Instant::now());
        if finished.recv_timeout(left).is_err() {
            break; // a reader still blocked on a grandchild's pipe: leave it
        }
    }
    Ok(Captured {
        ended,
        stdout: taken(&stdout),
        stderr: taken(&stderr),
    })
}

/// Read `pipe` to its end on a thread, into a shared [`Stream`], and say
/// so on `done`.
fn read(
    pipe: Option<impl Read + Send + 'static>,
    keep: usize,
    done: Sender<()>,
) -> io::Result<Arc<Mutex<Stream>>> {
    let stream = Arc::new(Mutex::new(Stream::default()));
    let shared = Arc::clone(&stream);
    thread::Builder::new().spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => lock(&shared).push(&chunk[..read], keep),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = done.send(());
    })?;
    Ok(stream)
}

fn lock(stream: &Mutex<Stream>) -> std::sync::MutexGuard<'_, Stream> {
    stream.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What's been read so far. A reader left blocked keeps counting into the
/// emptied stream, which nobody reads again.
fn taken(stream: &Mutex<Stream>) -> Stream {
    std::mem::take(&mut *lock(stream))
}

/// Wait for `child`, killing it at the `timeout`.
fn wait(child: &mut Child, timeout: Option<Duration>) -> io::Result<Ended> {
    let Some(timeout) = timeout else {
        return child.wait().map(Ended::Exited);
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ended::Exited(status));
        }
        if Instant::now() >= deadline {
            kill_group(child.id());
            let _ = child.kill(); // in case kill(1) couldn't
            child.wait()?;
            return Ok(Ended::TimedOut);
        }
        thread::sleep(POLL);
    }
}

/// The process groups of the commands running now: a few at most, as they
/// run one at a time. A signal to fussycat's own group never reaches them, so
/// [`stop_all`] passes it on.
static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn running() -> MutexGuard<'static, Vec<u32>> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A command in a process group of its own, whose id is its pid. It's in
/// [`RUNNING`] from its spawn until this is dropped, so an early return or a
/// panic still takes it out.
struct Group(u32);

impl Group {
    /// Spawn `command` in a group of its own, and register the group. Both
    /// happen under the lock, so [`stop_all`] can't fall between them.
    fn spawn(mut command: Command) -> io::Result<(Child, Self)> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut running = running();
        let child = command.spawn()?;
        let group = Self(child.id());
        running.push(group.0);
        Ok((child, group))
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        running().retain(|&group| group != self.0);
    }
}

/// Every running command's group killed, and no command can start, or
/// finish, while this is held: for a signal that ends fussycat.
#[cfg(unix)]
#[must_use = "commands start again once it's dropped"]
pub struct Stopped {
    _running: MutexGuard<'static, Vec<u32>>,
}

/// SIGKILL to every running command's group (see [`Stopped`]).
#[cfg(unix)]
pub fn stop_all() -> Stopped {
    let running = running();
    for &group in running.iter() {
        kill_group(group);
    }
    Stopped { _running: running }
}

/// SIGKILL to the whole process `group`, through kill(1): std signals only
/// the child itself, and the workspace forbids `unsafe` (so no libc). The
/// id can't have been reused while the leader is unreaped (a timeout) or a
/// job it left is in the group; past that, a group stays registered for at
/// most [`DRAIN_GRACE`], far too soon for the pids to wrap round. A failure
/// is ignored: on a timeout, [`Child::kill`] still gets the child.
#[cfg_attr(not(unix), allow(unused_variables))]
fn kill_group(group: u32) {
    #[cfg(unix)]
    {
        let group = format!("-{group}");
        let _ = isolated("kill", &[])
            .args(["-s", "KILL", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests;
