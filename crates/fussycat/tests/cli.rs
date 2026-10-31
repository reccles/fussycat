//! The exit-code and stdout contract, on the built binary: what a script
//! sees. Offline: the only server is a fake Ollama on loopback, and every
//! other run stops before any model call.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// fussycat with a clean environment: no settings of the developer's (their
/// model, their key commands) leak into the test.
fn fussycat(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fussycat"));
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_PROXY", "127.0.0.1,localhost");
    command
}

/// Run `command` with `task` on stdin.
fn run(mut command: Command, task: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(task.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A fresh, empty directory for one test.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fussycat-bin-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_bad_flag_exits_2() {
    let output = run(
        fussycat(&["--model", "ollama:m", "--max-turns", "lots"]),
        "hi",
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}

#[test]
fn a_missing_key_command_is_a_usage_error() {
    let output = run(fussycat(&["--model", "venice:m"]), "hi");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("FUSSYCAT_VENICE_KEY_COMMAND"));
}

/// The author's level reaches the provider (Ollama's `think`); a word
/// that isn't a level is a usage error, before any call.
#[test]
fn reasoning_effort_reaches_the_provider() {
    let (url, server) = fake_ollama("391");
    let mut command = answering(&url);
    command.args(["--reasoning-effort", "low"]);
    let output = run(command, "What is 17*23?");
    let asked = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let body: serde_json::Value = serde_json::from_str(&asked[0]).unwrap();
    assert_eq!(body["think"], "low", "{body}");

    let output = run(fussycat(&["--reasoning-effort", "hard"]), "hi");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("off, minimal, low"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_failing_key_command_is_a_failure() {
    let mut command = fussycat(&["--model", "venice:m"]);
    command.env("FUSSYCAT_VENICE_KEY_COMMAND", "echo locked >&2; false");
    let output = run(command, "hi");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("locked"), "{}", stderr(&output));
}

#[test]
fn an_empty_task_never_runs_the_key_command() {
    let dir = scratch("key-order");
    let asked = dir.join("asked-for-the-key");
    let with_key = |task: &str| {
        // --metrics can't be created: a run with a task fails there, after
        // the key, and never reaches the provider.
        let metrics = dir.join("no-such-dir/metrics.jsonl");
        let mut command = fussycat(&[
            "--model",
            "venice:m",
            "--metrics",
            metrics.to_str().unwrap(),
        ]);
        command.env(
            "FUSSYCAT_VENICE_KEY_COMMAND",
            format!("touch '{}' && echo key", asked.display()),
        );
        run(command, task)
    };

    let output = with_key("");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        !asked.exists(),
        "the key command ran for a run that can't start"
    );

    // The control: with a task, the key command runs, so the test can see it.
    let output = with_key("hi");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(asked.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// A fake Ollama on loopback that answers one `/api/chat` with `answer`.
fn fake_ollama(answer: &str) -> (String, JoinHandle<Vec<String>>) {
    fake_ollama_replying(vec![
        serde_json::json!({"role": "assistant", "content": answer}),
    ])
}

/// A fake Ollama that answers one `/api/chat` with each of `messages` in
/// turn. Joining it gives the request bodies it was sent.
fn fake_ollama_replying(messages: Vec<serde_json::Value>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut asked = Vec::new();
        for message in messages {
            let (stream, _) = listener.accept().unwrap();
            let mut request = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                request.read_line(&mut line).unwrap();
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; length];
            request.read_exact(&mut body).unwrap();
            asked.push(String::from_utf8(body).unwrap());
            let reply = serde_json::json!({
                "model": "m", "created_at": "2026-10-05T00:00:00Z",
                "done": true, "done_reason": "stop", "message": message,
            })
            .to_string();
            write!(
                request.into_inner(),
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
        asked
    });
    (url, server)
}

fn answering(url: &str) -> Command {
    fussycat(&[
        "--model",
        "ollama:m",
        "--ollama-url",
        url,
        "--retries",
        "0",
        "-q",
    ])
}

#[test]
fn the_answer_is_stdout_and_nothing_else() {
    let (url, server) = fake_ollama("It's noon.");
    let output = run(answering(&url), "What time is it?");
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout, b"It's noon.\n");
    assert_eq!(stderr(&output), "");
}

/// Stdout is the answer plus one trailing newline, never two, and nothing
/// at all when the run doesn't answer.
#[test]
fn stdout_gets_one_trailing_newline_and_nothing_without_an_answer() {
    let (url, server) = fake_ollama("It's noon.\n");
    let output = run(answering(&url), "What time is it?");
    server.join().unwrap();
    assert_eq!(output.stdout, b"It's noon.\n", "a newline isn't doubled");

    let (url, server) = fake_ollama_replying(vec![serde_json::json!({
        "role": "assistant", "content": "",
        "tool_calls": [{"function": {"name": "now", "arguments": {}}}]})]);
    let mut command = answering(&url);
    command.args(["--tool", "now=echo noon", "--max-turns", "1"]);
    let output = run(command, "What time is it?");
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "exhausted: no answer");

    let (url, server) = fake_ollama("");
    let output = run(answering(&url), "What time is it?");
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "failed: no answer");
}

/// A tool and a live command that both hang: each is killed at the
/// timeout, the model sees why, and the run goes on to its answer.
#[test]
fn a_hung_command_is_killed_and_the_run_goes_on() {
    let (url, server) = fake_ollama_replying(vec![
        serde_json::json!({"role": "assistant", "content": "",
            "tool_calls": [{"function": {"name": "hang", "arguments": {}}}]}),
        serde_json::json!({"role": "assistant", "content": "It hung."}),
    ]);
    let mut command = answering(&url);
    command.args([
        "--tool",
        "hang=sleep 30",
        "--live",
        "sleep 31",
        "--command-timeout",
        "0.2",
    ]);
    let started = Instant::now();
    let output = run(command, "Call hang.");
    let asked = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout, b"It hung.\n");
    assert!(started.elapsed() < Duration::from_secs(5));
    let second = &asked[1];
    assert!(
        second.contains("`sleep 30` timed out after 0.2s"),
        "the tool result: {second}"
    );
    assert!(
        second.contains(r#"status=\"failed\">\nfetch failed: `sleep 31` timed out after 0.2s"#),
        "the live block: {second}"
    );
}

/// A provider that takes the call and never answers: given up on at
/// `--provider-timeout`, a failure (exit 1) that says so, not a hang.
#[test]
fn a_silent_provider_is_given_up_on_at_the_provider_timeout() {
    // The OS accepts connections into the backlog; nothing ever answers.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());
    let mut command = answering(&url);
    command.args(["--provider-timeout", "0.2"]);
    let started = Instant::now();
    let output = run(command, "What time is it?");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("timed out"), "{}", stderr(&output));
    assert!(started.elapsed() < Duration::from_secs(5));

    let mut zero = answering(&url);
    zero.args(["--provider-timeout", "0"]);
    let refused = run(zero, "hi");
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
}

#[test]
fn a_reader_that_closes_the_pipe_early_gets_a_quiet_exit_not_a_panic() {
    let (url, server) = fake_ollama(&"a long answer\n".repeat(10_000));
    let mut child = answering(&url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the reading end before fussycat can write: `fussycat | head -c0`.
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"hi").unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stderr(&output), "", "quiet: no panic, no complaint");
}

// A stop signal (SIGINT, SIGTERM) kills the running command's whole
// process group and exits 128 + the signal, whether it's sent to fussycat
// alone (`kill`) or to its group (Ctrl-C, GNU `timeout`).

/// fussycat with `task` on stdin, in a process group of its own, so a test
/// can signal that group without signalling itself. When the test ends,
/// pass or fail, fussycat's group and the tool's are killed.
#[cfg(unix)]
struct Signalled {
    fussycat: std::process::Child,
    tool_group: Option<String>,
}

#[cfg(unix)]
impl Signalled {
    fn start(mut command: Command, task: &str) -> Self {
        use std::os::unix::process::CommandExt;
        let mut fussycat = command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = fussycat.stdin.take().unwrap();
        stdin.write_all(task.as_bytes()).unwrap();
        Self {
            fussycat,
            tool_group: None,
        }
    }

    /// `kill -s SIGNAL` to fussycat, or to its whole group.
    fn signal(&self, signal: &str, to_group: bool) {
        let pid = self.fussycat.id();
        let target = if to_group {
            format!("-{pid}")
        } else {
            pid.to_string()
        };
        assert!(
            kill(&["-s", signal, "--", &target]),
            "kill {signal} {target}"
        );
    }

    /// What fussycat printed, once it's exited; it gets `limit`.
    fn output_within(&mut self, limit: Duration) -> Output {
        let status = within(limit, "fussycat to exit", || {
            self.fussycat.try_wait().unwrap()
        });
        Output {
            status,
            stdout: drained(self.fussycat.stdout.take()),
            stderr: drained(self.fussycat.stderr.take()),
        }
    }
}

#[cfg(unix)]
fn drained(pipe: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    pipe.unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[cfg(unix)]
impl Drop for Signalled {
    fn drop(&mut self) {
        let fussycat = format!("-{}", self.fussycat.id());
        for group in std::iter::once(&fussycat).chain(&self.tool_group) {
            kill(&["-s", "KILL", "--", group]);
        }
        let _ = self.fussycat.wait();
    }
}

/// kill(1) with `args`: whether it succeeded. Quiet: a group that's gone
/// is often the point.
#[cfg(unix)]
fn kill(args: &[&str]) -> bool {
    Command::new("kill")
        .args(args)
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// What `ready` gives, polled until it gives something, for up to `limit`.
#[cfg(unix)]
fn within<T>(limit: Duration, what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(Instant::now() < deadline, "waited {limit:?} for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Long enough for a loaded machine; far shorter than the tool's sleep.
#[cfg(unix)]
const PROMPTLY: Duration = Duration::from_secs(5);

/// Signal fussycat while a tool runs: the tool writes its group's id to
/// `ready`, starts a job that would write `survived` a second later, and
/// sleeps. fussycat must exit `code` promptly, print no answer, and take the
/// whole group with it.
#[cfg(unix)]
fn assert_stops_the_running_tool(signal: &str, to_group: bool, code: i32) {
    let dir = scratch(&format!("signal-{signal}-{to_group}"));
    let ready = dir.join("ready");
    let survived = dir.join("survived");
    let (url, _server) = fake_ollama_replying(vec![serde_json::json!({
        "role": "assistant", "content": "",
        "tool_calls": [{"function": {"name": "nap", "arguments": {}}}]
    })]);
    let mut command = answering(&url);
    command.arg("--tool").arg(format!(
        "nap=(sleep 1; touch '{survived}') & echo $$ > '{ready}.tmp' && mv '{ready}.tmp' '{ready}'; sleep 30",
        survived = survived.display(),
        ready = ready.display(),
    ));
    let mut run = Signalled::start(command, "Call nap.");
    let group = within(PROMPTLY, "the tool to start", || {
        std::fs::read_to_string(&ready).ok()
    });
    let group = format!("-{}", group.trim());
    run.tool_group = Some(group.clone());
    assert!(
        kill(&["-0", "--", &group]),
        "the tool has a group of its own"
    );

    run.signal(signal, to_group);
    let output = run.output_within(PROMPTLY);
    assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "no answer");
    assert_eq!(
        stderr(&output),
        format!("fussycat: interrupted (SIG{signal})\n")
    );
    // The group is gone once its zombies are reaped; in a container whose
    // init doesn't reap they'd linger, so after a while settle for the job
    // never having done its work.
    let gone = Instant::now() + Duration::from_millis(1500);
    while kill(&["-0", "--", &group]) && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !survived.exists(),
        "a job the tool started outlived fussycat"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn sigterm_to_fussycat_kills_the_running_tool_and_exits_143() {
    assert_stops_the_running_tool("TERM", false, 143);
}

/// GNU `timeout 300 fussycat ...` signals its child's whole group.
#[cfg(unix)]
#[test]
fn sigterm_to_fussycats_group_like_timeout_kills_the_running_tool() {
    assert_stops_the_running_tool("TERM", true, 143);
}

/// Ctrl-C signals the terminal's foreground group.
#[cfg(unix)]
#[test]
fn sigint_to_fussycats_group_like_ctrl_c_kills_the_running_tool_and_exits_130() {
    assert_stops_the_running_tool("INT", true, 130);
}

#[cfg(unix)]
#[test]
fn sigint_to_fussycat_kills_the_running_tool() {
    assert_stops_the_running_tool("INT", false, 130);
}

/// `nohup fussycat ... &` asks for a hangup to be ignored, and fussycat
/// leaves SIGHUP alone, so it keeps running; SIGTERM still stops it.
#[cfg(unix)]
#[test]
fn an_ignored_sighup_stays_ignored_like_nohup_asks() {
    let dir = scratch("nohup");
    let ready = dir.join("ready");
    let (url, _server) = fake_ollama_replying(vec![serde_json::json!({
        "role": "assistant", "content": "",
        "tool_calls": [{"function": {"name": "nap", "arguments": {}}}]
    })]);
    let inner = answering(&url);
    // What nohup does: ignore SIGHUP, then exec, which keeps the ignore.
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("trap '' HUP; exec \"$0\" \"$@\"")
        .arg(inner.get_program())
        .args(inner.get_args())
        .env_clear();
    for (key, value) in inner.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    command.arg("--tool").arg(format!(
        "nap=echo $$ > '{ready}.tmp' && mv '{ready}.tmp' '{ready}'; sleep 30",
        ready = ready.display(),
    ));
    let mut run = Signalled::start(command, "Call nap.");
    let group = within(PROMPTLY, "the tool to start", || {
        std::fs::read_to_string(&ready).ok()
    });
    run.tool_group = Some(format!("-{}", group.trim()));

    run.signal("HUP", false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        run.fussycat.try_wait().unwrap().is_none(),
        "a hangup nohup ignores ended fussycat"
    );
    run.signal("TERM", false);
    let output = run.output_within(PROMPTLY);
    assert_eq!(output.status.code(), Some(143), "{}", stderr(&output));
    std::fs::remove_dir_all(dir).unwrap();
}

/// No command running: a signal mid model call ends the run there, rather
/// than after the call.
#[cfg(unix)]
#[test]
fn a_signal_between_commands_stops_the_run_at_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut run = Signalled::start(answering(&url), "hi");
    // Connected and never answered: fussycat is waiting on the model.
    let _call = within(PROMPTLY, "the model call", || listener.accept().ok());
    run.signal("TERM", false);
    let output = run.output_within(PROMPTLY);
    assert_eq!(output.status.code(), Some(143), "{}", stderr(&output));
    assert_eq!(output.stdout, b"", "no answer");
    assert_eq!(stderr(&output), "fussycat: interrupted (SIGTERM)\n");
}

/// A stderr nobody reads (its pipe's reader is gone) costs the message,
/// never the exit status: `eprintln!` would panic (101) instead of 2.
#[test]
fn a_broken_stderr_pipe_keeps_the_exit_status() {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = fussycat(&["--model", "ollama:m"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(writer)
        .spawn()
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(2), "a usage error");
}
