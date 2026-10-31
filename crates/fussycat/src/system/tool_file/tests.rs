use super::*;
use crate::system::now::InvokeNow;
use serde_json::json;

fn tool(text: &str) -> ToolFile {
    ToolFile::parse(text, Limits::DEFAULT).unwrap()
}

fn problem(text: &str) -> String {
    match ToolFile::parse(text, Limits::DEFAULT) {
        Ok(_) => panic!("expected a problem with:\n{text}"),
        Err(Problem::Invalid(message) | Problem::Help(message)) => message,
    }
}

const SCALE: &str = r#"
name = "scale_recipe"
description = """
Scales a recipe.
"""
run = ["printf", "%s|%s", "{recipe}", "{servings}"]

[input.servings]
type = "integer"
minimum = 1
maximum = 50

[input.recipe]
type = "string"
description = "Recipe name"
pattern = "^[a-z ]+$"
"#;

/// `version = 1` or none; a newer format is refused, not half-read.
#[test]
fn the_file_format_is_versioned() {
    let echo = "name = \"echo\"\ndescription = \"Echoes.\"\nrun = [\"echo\"]";
    tool(&format!("version = 1\n{echo}"));
    tool(echo);
    let newer = problem(&format!("version = 2\n{echo}"));
    assert!(
        newer.contains("`version = 2` is newer than this fussycat reads (1)"),
        "{newer}"
    );
}

#[test]
fn typed_inputs_become_a_strict_schema_in_a_fixed_order() {
    let scale = tool(SCALE);
    assert_eq!(scale.spec().name, "scale_recipe");
    assert_eq!(scale.spec().description, "Scales a recipe.");
    let schema = serde_json::to_string(&scale.spec().parameters).unwrap();
    assert_eq!(
        schema,
        r#"{"type":"object","properties":{"recipe":{"type":"string","description":"Recipe name","pattern":"^(?:^[a-z ]+$)$"},"servings":{"type":"integer","minimum":1,"maximum":50}},"required":["recipe","servings"],"additionalProperties":false}"#
    );
}

#[test]
fn inputs_fill_their_placeholders_as_whole_arguments() {
    let scale = tool(SCALE);
    let call = json!({"recipe": "pancakes", "servings": 4});
    assert_eq!(scale.invoke_now(&call), Ok("pancakes|4".into()));
}

#[test]
fn arch_model_text_never_reaches_a_shell() {
    let echo = tool(
        r#"
name = "echo"
description = "Echoes."
run = ["printf", "%s", "{text}"]
[input.text]
type = "string"
"#,
    );
    let hostile = "; touch /tmp/fussycat-pwned $(id) `id` | cat";
    assert_eq!(
        echo.invoke_now(&json!({"text": hostile})),
        Ok(hostile.into())
    );
}

/// A tool with one input `{text}`, declared by `input` (the TOML lines
/// after `[input.text]`), that prints what it was given. `--` because
/// uutils printf (Ubuntu 26.04's) reads `--version` as its own option
/// even after the format; GNU and BSD printf skip it.
fn echo(input: &str) -> ToolFile {
    tool(&format!(
        "name = \"echo\"\ndescription = \"d\"\nrun = [\"printf\", \"--\", \"%s\", \"{{text}}\"]\n[input.text]\n{input}"
    ))
}

#[test]
fn arch_model_text_never_becomes_an_option() {
    let echo = echo("type = \"string\"");
    for option in ["--upload-pack=touch /tmp/fussycat-pwned", "-o/tmp/x", "-"] {
        assert_eq!(
            echo.invoke_now(&json!({"text": option})),
            Err("`text` can't start with `-`: the program would read it as an option. Rephrase it without the leading `-`.".into()),
            "{option}"
        );
    }
    assert_eq!(echo.invoke_now(&json!({"text": "a -b"})), Ok("a -b".into()));
}

#[test]
fn an_input_can_allow_a_leading_dash() {
    let flag = echo("type = \"string\"\nallow_leading_dash = true");
    assert_eq!(
        flag.invoke_now(&json!({"text": "--version"})),
        Ok("--version".into())
    );
    let choice = echo("type = \"string\"\nenum = [\"-l\", \"-a\"]\nallow_leading_dash = true");
    assert_eq!(choice.invoke_now(&json!({"text": "-l"})), Ok("-l".into()));
}

/// `-5` is a value, not an option: numbers are typed, so they can't carry
/// an option name or a path. `minimum` is how an author refuses them.
#[test]
fn a_negative_number_is_a_value() {
    assert_eq!(
        echo("type = \"integer\"").invoke_now(&json!({"text": -5})),
        Ok("-5".into())
    );
    assert_eq!(
        echo("type = \"number\"").invoke_now(&json!({"text": -1.5})),
        Ok("-1.5".into())
    );
    assert_eq!(
        echo("type = \"integer\"\nminimum = 0").invoke_now(&json!({"text": -5})),
        Err("`text` must be at least 0".into())
    );
}

/// Unlike JSON Schema's, a `pattern` matches the whole value: it guards
/// argv, so `[a-z]+` must not accept `rm -rf /`.
#[test]
fn a_pattern_matches_the_whole_value() {
    let word = echo("type = \"string\"\npattern = \"[a-z]+\"\nallow_leading_dash = true");
    assert_eq!(
        word.invoke_now(&json!({"text": "pancakes"})),
        Ok("pancakes".into())
    );
    for partial in ["rm -rf /", "-rf /; x", "x\n-y"] {
        assert_eq!(
            word.invoke_now(&json!({"text": partial})),
            Err("`text` must match the pattern ^(?:[a-z]+)$".into()),
            "{partial:?}"
        );
    }
    assert_eq!(
        word.spec().parameters["properties"]["text"]["pattern"],
        "^(?:[a-z]+)$",
        "the model sees the pattern the guard uses"
    );
    let either = echo("type = \"string\"\npattern = \"cat|dog\"");
    assert!(either.invoke_now(&json!({"text": "dog"})).is_ok());
    assert!(either.invoke_now(&json!({"text": "catapult"})).is_err());
}

#[test]
fn arguments_are_checked_before_anything_runs() {
    let scale = tool(SCALE);
    let error = |call: Value| scale.invoke_now(&call).unwrap_err();
    assert_eq!(
        error(json!({"recipe": "pancakes"})),
        "missing argument `servings`"
    );
    assert_eq!(
        error(json!({"recipe": "x", "servings": 2, "salt": true})),
        "unknown argument `salt`: this tool takes recipe, servings"
    );
    assert_eq!(
        error(json!({"recipe": "x", "servings": "4"})),
        "`servings`: expected integer, got \"4\""
    );
    assert_eq!(
        error(json!({"recipe": "x", "servings": 0})),
        "`servings` must be at least 1"
    );
    assert_eq!(
        error(json!({"recipe": "x", "servings": 51})),
        "`servings` must be at most 50"
    );
    assert_eq!(
        error(json!({"recipe": "rm -rf ~", "servings": 2})),
        "`recipe` must match the pattern ^(?:^[a-z ]+$)$"
    );
    assert_eq!(
        error(json!(["pancakes"])),
        "arguments must be a JSON object, got [\"pancakes\"]"
    );
}

#[test]
fn enum_and_boolean_inputs() {
    let pick = tool(
        r#"
name = "pick"
description = "Picks."
run = ["printf", "%s %s", "{size}", "{hot}"]
[input.size]
type = "string"
enum = ["small", "large"]
[input.hot]
type = "boolean"
"#,
    );
    assert_eq!(
        pick.invoke_now(&json!({"size": "large", "hot": true})),
        Ok("large true".into())
    );
    assert_eq!(
        pick.invoke_now(&json!({"size": "huge", "hot": true})),
        Err("`size` must be one of \"small\", \"large\"".into())
    );
}

const BROAD: &str = r#"
name = "show"
help = ["sh", "-c", "echo 'Usage: show [WORDS]'; echo; echo '  Prints each word in brackets.   '"]
run = ["printf", "--", "[%s]", "{args...}"]
"#;

#[test]
fn help_text_describes_the_tool_and_the_model_supplies_the_arguments() {
    let show = tool(BROAD);
    assert_eq!(
        show.spec().description,
        "`sh -c echo 'Usage: show [WORDS]'; echo; echo '  Prints each word in brackets.   '` says:\nUsage: show [WORDS]\n\n  Prints each word in brackets."
    );
    assert_eq!(show.spec().parameters["required"], json!(["args"]));
    assert_eq!(
        show.invoke_now(&json!({"args": ["a b", "$(id)"]})),
        Ok("[a b][$(id)]".into())
    );
    assert_eq!(
        show.invoke_now(&json!({"args": "a b"})),
        Err("`args` must be a list of strings".into())
    );
}

/// `{args...}` is the broad grant: any invocation, options included.
#[test]
fn the_broad_shape_passes_options_through() {
    assert_eq!(
        tool(BROAD).invoke_now(&json!({"args": ["--version", "-o/tmp/x"]})),
        Ok("[--version][-o/tmp/x]".into())
    );
}

#[test]
fn a_description_comes_before_the_help_text() {
    let both = tool(
        r#"
name = "both"
description = "Use for X."
help = ["printf", "Usage: both"]
run = ["true", "{args...}"]
"#,
    );
    assert_eq!(
        both.spec().description,
        "Use for X.\n\n`printf Usage: both` says:\nUsage: both"
    );
}

#[test]
fn help_runs_the_same_way_every_time() {
    let env = tool(
        r#"
name = "env"
help = ["sh", "-c", "echo \"$LC_ALL $COLUMNS $NO_COLOR $TERM\""]
run = ["true"]
"#,
    );
    assert!(
        env.spec().description.ends_with("\nC 80 1 dumb"),
        "{}",
        env.spec().description
    );
}

/// Cargo sets `CARGO_MANIFEST_DIR` for every test it runs, so it stands in
/// for a credential fussycat has and a tool might want.
const PRESENT: &str = "CARGO_MANIFEST_DIR";

#[test]
fn only_variables_the_file_names_reach_the_program() {
    assert!(std::env::var_os(PRESENT).is_some(), "run under cargo");
    let printenv = |env: &str| {
        tool(&format!(
            "name = \"x\"\ndescription = \"d\"\nrun = [\"sh\", \"-c\", \"echo ${{{PRESENT}:-unset}}\"]\n{env}"
        ))
        .invoke_now(&json!({}))
        .unwrap()
    };
    assert_eq!(printenv(""), "unset\n", "not passed unless named");
    assert_eq!(
        printenv(&format!("env = [\"{PRESENT}\"]")),
        format!("{}\n", env!("CARGO_MANIFEST_DIR"))
    );
}

#[test]
fn the_help_command_gets_the_same_environment() {
    let help = |env: &str| {
        tool(&format!(
            "name = \"x\"\nhelp = [\"sh\", \"-c\", \"echo ${{{PRESENT}:-unset}}\"]\nrun = [\"true\"]\n{env}"
        ))
        .spec()
        .description
        .clone()
    };
    assert!(help("").ends_with("\nunset"), "{}", help(""));
    let passed = help(&format!("env = [\"{PRESENT}\"]"));
    assert!(passed.ends_with(env!("CARGO_MANIFEST_DIR")), "{passed}");
}

#[test]
fn help_on_stderr_with_a_failing_exit_still_counts() {
    let usage = tool(
        r#"
name = "usage"
help = ["sh", "-c", "echo 'usage: x' >&2; exit 2"]
run = ["true"]
"#,
    );
    assert!(usage.spec().description.ends_with("\nusage: x"));
}

#[test]
fn long_help_is_cut_visibly() {
    let long = tool(
        r#"
name = "long"
help = ["sh", "-c", "yes 'all work and no play' | head -c 20000"]
run = ["true"]
"#,
    );
    let description = &long.spec().description;
    assert!(
        description.len() < HELP_LIMIT + 200,
        "{}",
        description.len()
    );
    assert!(
        // Read only up to the cap, so cut within it, but still the true size.
        description.ends_with("[help cut: showing the first 8142 of 20000 bytes]"),
        "{}",
        &description[description.len() - 60..]
    );
}

#[test]
fn a_help_command_that_cannot_run_is_a_help_problem() {
    let missing = r#"
name = "missing"
help = ["fussycat-no-such-program-x"]
run = ["true"]
"#;
    assert!(matches!(
        ToolFile::parse(missing, Limits::DEFAULT),
        Err(Problem::Help(_))
    ));
    let silent = r#"
name = "silent"
help = ["true"]
run = ["true"]
"#;
    assert_eq!(problem(silent), "`true` printed nothing");
}

#[test]
fn bad_declarations_are_refused_with_the_reason() {
    let cases = [
        (
            "name = \"x\"\ndiscription = \"typo\"\nrun = [\"true\"]",
            "unknown field `discription`",
        ),
        (
            "name = \"x\"\nrun = [\"true\"]",
            "needs a `description`, a `help` command, or both",
        ),
        (
            "name = \"my tool\"\ndescription = \"d\"\nrun = [\"true\"]",
            "name `my tool` must be",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = []",
            "`run` is empty",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"true\"]\nenv = [\"API KEY\"]",
            "`env` entry `API KEY` must be a variable name",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"true\"]\nenv = [\"1PASSWORD\"]",
            "`env` entry `1PASSWORD` must be a variable name",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"{cmd}\"]\n[input.cmd]\ntype = \"string\"",
            "the program (the first item of `run`) must be fixed",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{text}\"]",
            "`{text}` in `run` has no [input.text]",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\"]\n[input.text]\ntype = \"string\"",
            "[input.text] is never used in `run`",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"cook\", \"-p{dish}\"]\n[input.dish]\ntype = \"string\"",
            "a placeholder must be a whole argument, like \"{dish}\"",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{args...}\", \"{args...}\"]",
            "can appear only once",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{a}\", \"{args...}\"]\n[input.a]\ntype = \"string\"",
            "can't be combined with [input] tables",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{n}\"]\n[input.n]\ntype = \"integer\"\npattern = \"^1$\"",
            "`pattern` only applies to strings",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{s}\"]\n[input.s]\ntype = \"string\"\nminimum = 1",
            "`minimum` and `maximum` only apply to numbers",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{s}\"]\n[input.s]\ntype = \"string\"\npattern = \"(\"",
            "bad pattern",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{s}\"]\n[input.s]\ntype = \"string\"\npattern = \"a)|(b\"",
            "bad pattern",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{n}\"]\n[input.n]\ntype = \"integer\"\nallow_leading_dash = true",
            "`allow_leading_dash` only applies to strings",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"ls\", \"{s}\"]\n[input.s]\ntype = \"string\"\nenum = [\"-l\"]",
            "`enum` choice \"-l\" starts with `-`: set `allow_leading_dash = true`",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{s}\"]\n[input.s]\ntype = \"string\"\nenum = [1]",
            "`enum`: expected string, got 1",
        ),
        (
            "name = \"x\"\ndescription = \"d\"\nrun = [\"echo\", \"{s}\"]\n[input.s]\ntype = \"list\"",
            "unknown variant `list`",
        ),
    ];
    for (text, expected) in cases {
        let message = problem(text);
        assert!(
            message.contains(expected),
            "{expected:?} not in {message:?}"
        );
    }
}

#[test]
fn literal_braces_that_are_not_placeholders_pass_through() {
    let jq = tool(
        r#"
name = "jq_keys"
description = "d"
run = ["printf", "%s", "{a: .b}"]
"#,
    );
    assert_eq!(jq.invoke_now(&json!({})), Ok("{a: .b}".into()));
    assert_eq!(
        jq.invoke_now(&json!({"x": 1})),
        Err("unknown argument `x`: this tool takes no arguments".into())
    );
}

#[test]
fn a_failing_program_is_an_err_result_with_its_output() {
    let fail = tool(
        r#"
name = "fail"
description = "d"
run = ["sh", "-c", "echo oops >&2; exit 3"]
"#,
    );
    let error = fail.invoke_now(&json!(null)).unwrap_err();
    assert!(error.contains("oops"), "{error}");
}

#[test]
fn errors_name_the_file_and_say_whether_it_was_a_usage_error() {
    let path = Path::new("/no/such/tools/x.toml");
    let unreadable = ToolFile::load(path, Limits::DEFAULT).err().unwrap();
    assert!(!unreadable.is_usage());
    assert!(
        unreadable
            .to_string()
            .starts_with("tool file `/no/such/tools/x.toml`: ")
    );
    assert!(Problem::Invalid("bad".into()).at(path).is_usage());
    assert!(!Problem::Help("broke".into()).at(path).is_usage());
}
