//! Against a fake MCP server (`tests/fixtures/fake_mcp.py`, standard-library
//! Python): a board with a read-only tool, a destructive one, a slow one,
//! one returning an image, and a resource.

use std::time::{Duration, Instant};

use serde_json::json;

use super::*;

const BRIEF: Limits = Limits {
    timeout: Duration::from_millis(500),
    ..Limits::DEFAULT
};

fn fake() -> String {
    format!("{}/tests/fixtures/fake_mcp.py", env!("CARGO_MANIFEST_DIR"))
}

/// A file for the fake server, with `rest` after its `name` and `run`.
fn board(rest: &str) -> String {
    format!(
        "name = \"board\"\nrun = [\"python3\", \"-I\", \"{}\"]\n{rest}",
        fake()
    )
}

fn parsed(text: &str, limits: Limits) -> Arc<McpServer> {
    Arc::new(McpServer::parse(text, limits).unwrap())
}

async fn started(text: &str) -> (Arc<McpServer>, Vec<Box<dyn Tool>>) {
    let server = parsed(text, Limits::DEFAULT);
    let tools = server.start().await.unwrap();
    (server, tools)
}

/// A server and its tools live on the runtime that started them, so each
/// test runs on one.
async fn call(tool: &dyn Tool, arguments: Value) -> Result<String, String> {
    tool.invoke(&arguments).await
}

fn problem(text: &str) -> String {
    McpServer::parse(text, Limits::DEFAULT).err().unwrap()
}

fn names(tools: &[Box<dyn Tool>]) -> Vec<&str> {
    tools.iter().map(|tool| tool.spec().name.as_str()).collect()
}

#[test]
fn the_allowlist_is_required_and_star_stands_alone() {
    assert!(problem(&board("")).contains("missing field `tools`"));
    assert!(problem(&board("tools = [\"*\", \"move_card\"]")).contains("stands alone"));
    assert!(problem(&board("tools = []\ntypo = 1")).contains("unknown field `typo`"));
}

#[test]
fn a_server_name_must_make_tool_names() {
    let file = "name = \"my__board\"\nrun = [\"x\"]\ntools = []";
    assert!(problem(file).contains("without `__`"), "{}", problem(file));
    assert!(problem("name = \"b\"\nrun = []\ntools = []").contains("`run` must start"));
    let env = "name = \"b\"\nrun = [\"x\"]\ntools = []\nenv = [\"NOT-A-NAME\"]";
    assert!(problem(env).contains("`env` entry `NOT-A-NAME`"));
}

#[test]
fn a_view_is_a_tool_with_arguments_or_a_resource() {
    let both = board("tools = []\n[live.state]\ntool = \"a\"\nresource = \"b://c\"");
    assert!(problem(&both).contains("not both"), "{}", problem(&both));
    let neither = board("tools = []\n[live.state]\n");
    assert!(problem(&neither).contains("needs `tool`"));
    let stray = board("tools = []\n[live.state]\nresource = \"b://c\"\narguments = {}");
    assert!(problem(&stray).contains("`arguments` go with `tool`"));
}

/// Each allowed tool, prefixed with the server's name, with the server's
/// own description and schema. The rest stay with the server.
#[tokio::test(flavor = "current_thread")]
async fn only_allowed_tools_are_granted_with_the_servers_prefix() {
    let (_server, tools) = started(&board("tools = [\"get_current_state\"]")).await;
    assert_eq!(names(&tools), ["board__get_current_state"]);
    let spec = tools[0].spec();
    assert_eq!(spec.description, "The board's current state.");
    assert_eq!(spec.parameters["properties"]["team"]["type"], "string");

    let (_server, every) = started(&board("tools = [\"*\"]")).await;
    assert_eq!(every.len(), 4);
}

/// A typo in the allowlist, or a server that dropped a tool, fails before
/// any model call, and says what the server does offer.
#[tokio::test(flavor = "current_thread")]
async fn an_allowed_tool_the_server_lacks_fails_the_start() {
    let server = parsed(&board("tools = [\"get_state\"]"), Limits::DEFAULT);
    let error = server.start().await.err().unwrap();
    assert!(!error.is_usage(), "the world's: the file was well formed");
    let error = error.to_string();
    assert!(
        error.starts_with("MCP server `board`: it offers no tool `get_state` (it offers: get_current_state, move_card"),
        "{error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_passes_the_models_arguments_and_returns_the_text() {
    let (_server, tools) = started(&board("tools = [\"get_current_state\", \"move_card\"]")).await;
    let state = &tools[0];
    assert_eq!(
        call(state.as_ref(), json!({"team": "core"})).await,
        Ok("team core: {\"in_progress\": 3, \"blocked\": 1}".into())
    );
    assert_eq!(
        call(state.as_ref(), json!(null)).await,
        Ok("team all: {\"in_progress\": 3, \"blocked\": 1}".into()),
        "no arguments: an empty object"
    );
    assert_eq!(
        call(state.as_ref(), json!(["core"])).await,
        Err("arguments must be an object, got [\"core\"]".into())
    );
}

/// The server's own error result is an error the model sees, as a
/// failing command's is.
#[tokio::test(flavor = "current_thread")]
async fn an_error_result_is_an_err() {
    let (_server, tools) = started(&board("tools = [\"move_card\"]")).await;
    assert_eq!(
        call(tools[0].as_ref(), json!({"card": 7, "to": "nowhere"})).await,
        Err("no column `nowhere`".into())
    );
}

/// What the model can't read is named, not dropped silently.
#[tokio::test(flavor = "current_thread")]
async fn content_the_model_cant_see_is_noted() {
    let (_server, tools) = started(&board("tools = [\"snapshot\"]")).await;
    assert_eq!(
        call(tools[0].as_ref(), json!({})).await,
        Ok("the board:\n[an image (image/png), not shown]".into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_past_the_timeout_is_an_err() {
    let server = parsed(&board("tools = [\"wait\"]"), BRIEF);
    let tools = server.start().await.unwrap();
    let started = Instant::now();
    assert_eq!(
        call(tools[0].as_ref(), json!({})).await,
        Err("`board__wait` timed out after 0.5s".into())
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// A live view runs the author's call, with the author's arguments, every
/// fetch: the model can't change them, and needn't be granted the tool.
#[tokio::test(flavor = "current_thread")]
async fn a_tool_view_fetches_with_the_authors_arguments() {
    let file = board(
        "tools = [\"move_card\"]\n\
         [live.state]\ntool = \"get_current_state\"\narguments = { team = \"core\" }",
    );
    let (server, tools) = started(&file).await;
    assert_eq!(
        names(&tools),
        ["board__move_card"],
        "the view's tool isn't granted"
    );
    let view = server.view("state").unwrap();
    assert_eq!(
        view.fetch().await,
        Ok("team core: {\"in_progress\": 3, \"blocked\": 1}".into())
    );
    call(tools[0].as_ref(), json!({"card": 1, "to": "doing"}))
        .await
        .unwrap();
    assert_eq!(
        view.fetch().await,
        Ok("team core: {\"in_progress\": 4, \"blocked\": 1}".into()),
        "fetched again, it shows what changed"
    );
    assert!(server.view("nope").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_resource_view_reads_its_uri() {
    let file = board("tools = []\n[live.board]\nresource = \"board://current\"");
    let (server, tools) = started(&file).await;
    assert!(tools.is_empty() && !server.grants_tools(), "views alone");
    assert_eq!(
        server.view("board").unwrap().fetch().await,
        Ok("{\"in_progress\": 3, \"blocked\": 1}".into())
    );
}

/// A view runs every turn: binding one to a tool the server itself marks
/// destructive is refused before the run starts.
#[tokio::test(flavor = "current_thread")]
async fn a_view_of_a_destructive_tool_is_refused() {
    let file = board("tools = []\n[live.oops]\ntool = \"move_card\"");
    let server = parsed(&file, Limits::DEFAULT);
    let error = server.start().await.err().unwrap().to_string();
    assert!(
        error.contains("live view `oops`: the server marks `move_card` destructive"),
        "{error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_view_is_cut_to_the_output_limit() {
    let limits = Limits {
        keep: 30,
        ..Limits::DEFAULT
    };
    let server = parsed(
        &board("tools = []\n[live.state]\ntool = \"get_current_state\""),
        limits,
    );
    server.start().await.unwrap();
    let content = server.view("state").unwrap().fetch().await.unwrap();
    assert!(content.contains("[output cut"), "{content}");
    assert!(content.len() <= 30 + 60, "{content}");
}

/// A server that dies at start fails with what it said on stderr.
#[tokio::test(flavor = "current_thread")]
async fn a_server_that_wont_start_says_why() {
    let file = "name = \"broken\"\n\
                run = [\"python3\", \"-I\", \"-c\", \"import sys; sys.stderr.write('no token set'); sys.exit(1)\"]\n\
                tools = []";
    let server = parsed(file, Limits::DEFAULT);
    let error = server.start().await.err().unwrap().to_string();
    assert!(error.starts_with("MCP server `broken`: "), "{error}");
    assert!(error.ends_with(": no token set"), "{error}");
}

/// The server and everything it started go when the last tool or view is
/// dropped, as a command's group does at its timeout. A job the server's
/// shell started proves it by never writing its file.
#[tokio::test(flavor = "current_thread")]
async fn dropping_the_server_kills_what_it_started() {
    let proof = std::env::temp_dir().join(format!("fussycat-mcp-survived-{}", std::process::id()));
    let _ = std::fs::remove_file(&proof);
    let file = format!(
        "name = \"board\"\n\
         run = [\"sh\", \"-c\", \"(sleep 0.6; touch '{}') & exec python3 -I '{}'\"]\n\
         tools = [\"get_current_state\"]",
        proof.display(),
        fake()
    );
    let began = Instant::now();
    let (server, tools) = started(&file).await;
    assert!(
        call(tools[0].as_ref(), json!({})).await.is_ok(),
        "it was up"
    );
    drop((server, tools));
    tokio::time::sleep(Duration::from_millis(1200).saturating_sub(began.elapsed())).await;
    let survived = proof.exists();
    let _ = std::fs::remove_file(&proof);
    assert!(!survived, "the server's job outlived it");
}

#[tokio::test(flavor = "current_thread")]
async fn listing_shows_every_tool_with_its_marks() {
    let server = parsed(&board("tools = []"), Limits::DEFAULT);
    let lines = server.offered().await.unwrap();
    assert_eq!(
        lines,
        [
            "get_current_state (read-only): The board's current state.",
            "move_card (destructive): Moves a card to a column.",
            "wait: Waits five seconds.",
            "snapshot: A picture of the board.",
        ]
    );
}
