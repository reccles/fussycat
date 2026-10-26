use super::*;

/// A parent environment holding the basics plus the kinds of secret a
/// shell tends to carry.
fn parent(name: &str) -> Option<OsString> {
    match name {
        "PATH" => std::env::var_os("PATH"),
        "HOME" => Some("/home/user".into()),
        "VENICE_API_KEY" => Some("venice-secret".into()),
        "GITHUB_TOKEN" => Some("github-secret".into()),
        _ => None,
    }
}

fn child_env(passed: &[&str]) -> String {
    let passed: Vec<String> = passed.iter().map(|&name| name.to_owned()).collect();
    output_of(
        isolated_from("env", &passed, parent),
        "env",
        Limits::DEFAULT,
    )
    .unwrap()
}

#[test]
fn arch_children_never_see_credentials() {
    let env = child_env(&[]);
    assert!(env.contains("HOME=/home/user"), "{env}");
    assert!(!env.contains("secret"), "a credential leaked:\n{env}");
    let names: Vec<&str> = env
        .lines()
        .filter_map(|line| line.split('=').next())
        .collect();
    assert!(
        names.iter().all(|name| INHERITED.contains(name)),
        "only the basics: {names:?}"
    );
}

#[test]
fn a_named_variable_is_passed_and_no_other() {
    let env = child_env(&["VENICE_API_KEY", "NOT_SET_ANYWHERE"]);
    assert!(env.contains("VENICE_API_KEY=venice-secret"), "{env}");
    assert!(!env.contains("github-secret"), "{env}");
    assert!(
        !env.contains("NOT_SET_ANYWHERE"),
        "unset stays unset:\n{env}"
    );
}

/// Short enough to keep the suite fast, long enough for a program to start.
const SHORT: Duration = Duration::from_millis(300);

/// `argv` run within `timeout` and `keep`, and how long that took.
fn run(argv: &[&str], timeout: Duration, keep: usize) -> (Result<String, String>, Duration) {
    let mut command = isolated(argv[0], &[]);
    command.args(&argv[1..]);
    let started = Instant::now();
    let result = output_of(command, &argv.join(" "), Limits { timeout, keep });
    (result, started.elapsed())
}

#[test]
fn a_command_past_its_timeout_is_killed_and_says_so() {
    let (result, took) = run(&["sleep", "30"], SHORT, 1024);
    assert_eq!(result, Err("`sleep 30` timed out after 0.3s: ".into()));
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

/// Only `keep` bytes are held, however much the command prints; the rest
/// is counted, so the note still gives the true size.
#[test]
fn output_past_the_cap_is_counted_not_kept() {
    let (result, _) = run(&["head", "-c", "1000000", "/dev/zero"], SHORT * 10, 100);
    let output = result.unwrap();
    assert_eq!(
        output,
        format!(
            "{}\n[output cut: showing the first 47 of 1000000 bytes]",
            "\0".repeat(47)
        )
    );
    assert!(output.len() <= 100, "{} bytes kept", output.len());
}

/// `yes` never stops: it's bounded in memory by the cap, and in time by
/// the timeout, and the model sees what it printed, cut.
#[test]
fn a_command_that_never_stops_printing_is_bounded() {
    let (result, took) = run(&["yes"], SHORT, 200);
    let error = result.unwrap_err();
    assert!(
        error.starts_with("`yes` timed out after 0.3s: y\ny\n"),
        "{error}"
    );
    assert!(error.len() <= 200, "{} bytes", error.len());
    assert!(error.contains("[error cut: showing the first"), "{error}");
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn a_timeout_is_shown_as_given() {
    assert_eq!(seconds(Limits::DEFAULT_TIMEOUT), "60s");
    assert_eq!(seconds(Duration::from_millis(250)), "0.25s");
}

#[test]
fn a_command_in_its_own_group_is_registered_until_it_is_dropped() {
    let mut command = isolated("sleep", &[]);
    command.arg("30");
    let (mut child, group) = Group::spawn(command).unwrap();
    let id = child.id();
    assert!(running().contains(&id), "registered while it runs");
    drop(group); // as on an early return or a panic
    assert!(!running().contains(&id), "and not after");
    child.kill().unwrap();
    child.wait().unwrap();
}
