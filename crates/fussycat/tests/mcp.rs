//! MCP servers on the built binary: `--mcp`, `--live mcp:SERVER.VIEW` and
//! `--list-tools`, against the fake server in `tests/fixtures/fake_mcp.py`
//! (standard-library Python) and a fake Ollama.

mod support;

use support::{answering, fake_ollama_replying, fussycat, run, scratch, stderr};

/// A file for the fake MCP server (`tests/fixtures/fake_mcp.py`) in `dir`.
fn board_file(dir: &std::path::Path, rest: &str) -> String {
    let path = dir.join("board.toml");
    let fake = format!("{}/tests/fixtures/fake_mcp.py", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        &path,
        format!("name = \"board\"\nrun = [\"python3\", \"-I\", \"{fake}\"]\n{rest}"),
    )
    .unwrap();
    path.display().to_string()
}

/// An MCP server's tool called by the model, and its live view shown every
/// turn: the second request has both the call's result and the state the
/// call changed.
#[test]
fn an_mcp_tool_and_live_view_reach_the_model() {
    let dir = scratch("mcp");
    let file = board_file(
        &dir,
        "tools = [\"move_card\"]\n[live.state]\ntool = \"get_current_state\"\narguments = { team = \"core\" }",
    );
    let (url, server) = fake_ollama_replying(vec![
        serde_json::json!({"role": "assistant", "content": "", "tool_calls": [{"function":
            {"name": "board__move_card", "arguments": {"card": 7, "to": "done"}}}]}),
        serde_json::json!({"role": "assistant", "content": "Moved."}),
    ]);
    let mut command = answering(&url);
    command.args(["--mcp", &file, "--live", "mcp:board.state"]);
    let output = run(command, "Move card 7 to done.");
    let asked = server.join().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout, b"Moved.\n");
    let (first, second) = (&asked[0], &asked[1]);
    assert!(first.contains(r#""name":"board__move_card""#), "{first}");
    assert!(
        !first.contains("get_current_state\""),
        "the view's tool isn't granted: {first}"
    );
    assert!(
        first.contains(r#"in_progress\": 3"#),
        "the view, before: {first}"
    );
    assert!(
        second.contains("moved card 7 to done"),
        "the result: {second}"
    );
    assert!(
        second.contains(r#"in_progress\": 2"#),
        "the view, after: {second}"
    );
}

/// A live view naming a server or view that isn't there is a usage error,
/// found before anything starts.
#[test]
fn an_unknown_mcp_view_is_a_usage_error() {
    let dir = scratch("mcp-usage");
    let file = board_file(
        &dir,
        "tools = []\n[live.state]\ntool = \"get_current_state\"",
    );
    for (live, says) in [
        (
            "mcp:nope.state",
            "no MCP server `nope` is granted with --mcp (granted: board)",
        ),
        (
            "mcp:board.nope",
            "MCP server `board` declares no [live.nope] (it declares: state)",
        ),
    ] {
        let mut command = fussycat(&["--model", "ollama:m", "--mcp", &file, "--live", live]);
        command.args(["--ollama-url", "http://127.0.0.1:9"]);
        let output = run(command, "hi");
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(stderr(&output).contains(says), "{}", stderr(&output));
    }
}

#[test]
fn list_tools_shows_what_a_server_offers() {
    let dir = scratch("mcp-list");
    let file = board_file(&dir, "tools = []");
    let output = run(fussycat(&["--list-tools", &file]), "");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let listed = String::from_utf8(output.stdout).unwrap();
    assert!(
        listed.starts_with(
            "get_current_state (read-only): The board's current state.\n\
             move_card (destructive): Moves a card to a column.\n"
        ),
        "{listed}"
    );
}
