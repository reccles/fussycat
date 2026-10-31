//! What the integration tests share: fussycat with a clean environment, a
//! run with a task on stdin, and a fake Ollama on loopback.
#![allow(dead_code)] // each test file uses its own share

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;

/// fussycat with a clean environment: no settings of the developer's (their
/// model, their key commands) leak into the test.
pub fn fussycat(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fussycat"));
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_PROXY", "127.0.0.1,localhost");
    command
}

/// Run `command` with `task` on stdin.
pub fn run(mut command: Command, task: &str) -> Output {
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

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A fresh, empty directory for one test.
pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fussycat-bin-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fake Ollama on loopback that answers one `/api/chat` with `answer`.
pub fn fake_ollama(answer: &str) -> (String, JoinHandle<Vec<String>>) {
    fake_ollama_replying(vec![
        serde_json::json!({"role": "assistant", "content": answer}),
    ])
}

/// A fake Ollama that answers one `/api/chat` with each of `messages` in
/// turn. Joining it gives the request bodies it was sent.
pub fn fake_ollama_replying(messages: Vec<serde_json::Value>) -> (String, JoinHandle<Vec<String>>) {
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

pub fn answering(url: &str) -> Command {
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
