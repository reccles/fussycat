//! Tools declared in a TOML file (`--tool tools/x.toml`): what the tool does
//! (the author's description, the program's own help text, or both) and how
//! to run it. The program runs from an argv list, never through a shell, so
//! model text only ever arrives as whole arguments, and a typed string never
//! as an option (a leading `-`) unless its input allows it. Like every child,
//! it gets a clean environment; `env = [...]` passes the named variables on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fussycat_core::toolbox::{Tool, ToolSpec, cap};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Number, Value, json};

use super::is_tool_name;
use super::process::{Ended, Limits, bounded, capture, isolated, output_of, seconds};

/// Help text past this many bytes is cut, visibly: it rides in every request.
const HELP_LIMIT: usize = 8 * 1024;
/// The placeholder that lets the model supply the arguments itself.
const REST: &str = "{args...}";

#[derive(Debug, thiserror::Error)]
pub enum ToolFileError {
    #[error("tool file `{}`: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("tool file `{}`: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("tool file `{}`: help: {message}", path.display())]
    Help { path: PathBuf, message: String },
}

impl ToolFileError {
    /// A file that's wrong is a usage error (exit 2). One the world failed
    /// to provide, unreadable or with a help command that won't run, is a
    /// failure (exit 1), like `cat /nope`.
    #[must_use]
    pub fn is_usage(&self) -> bool {
        matches!(self, Self::Invalid { .. })
    }
}

/// What's wrong, before we know which file it's in.
#[derive(Debug)]
enum Problem {
    Invalid(String),
    Help(String),
}

impl Problem {
    fn at(self, path: &Path) -> ToolFileError {
        let path = path.to_owned();
        match self {
            Self::Invalid(message) => ToolFileError::Invalid { path, message },
            Self::Help(message) => ToolFileError::Help { path, message },
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::Invalid(message.into()))
}

/// The file as written. Unknown keys are refused, so a typo like
/// `discription` is an error rather than a silently missing description.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    name: String,
    description: Option<String>,
    help: Option<Vec<String>>,
    run: Vec<String>,
    /// Variables passed on from fussycat's environment, e.g. a child fussycat's
    /// provider key. Everything else stays behind.
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    input: BTreeMap<String, InputDecl>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputDecl {
    #[serde(rename = "type")]
    kind: Kind,
    description: Option<String>,
    pattern: Option<String>,
    minimum: Option<Number>,
    maximum: Option<Number>,
    #[serde(rename = "enum")]
    choices: Option<Vec<Value>>,
    /// Lets a string start with `-`. Off by default: the program would read
    /// such a value as an option (`--upload-pack=...`, `-o/path`), not data.
    #[serde(default)]
    allow_leading_dash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    String,
    Integer,
    Number,
    Boolean,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
        }
    }

    /// `value` as one command-line argument, if it has this type.
    fn render(self, value: &Value) -> Result<String, String> {
        let rendered = match self {
            Self::String => value.as_str().map(str::to_owned),
            Self::Integer => (value.is_i64() || value.is_u64()).then(|| value.to_string()),
            Self::Number => value.is_number().then(|| value.to_string()),
            Self::Boolean => value.as_bool().map(|flag| flag.to_string()),
        };
        rendered.ok_or_else(|| format!("expected {}, got {value}", self.name()))
    }
}

/// One item of `run`.
#[derive(Debug, PartialEq)]
enum Arg {
    Fixed(String),
    Input(String),
    Rest,
}

/// A typed input, checked before the program runs.
struct Input {
    kind: Kind,
    pattern: Option<Regex>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    choices: Option<Vec<Value>>,
    allow_leading_dash: bool,
}

impl Input {
    fn compile(name: &str, decl: &InputDecl) -> Result<Self, Problem> {
        if !is_input_name(name) {
            return invalid(format!(
                "[input.{name}]: an input name is a letter or `_`, then letters, digits or `_`"
            ));
        }
        if decl.pattern.is_some() && decl.kind != Kind::String {
            return invalid(format!("[input.{name}]: `pattern` only applies to strings"));
        }
        if decl.allow_leading_dash && decl.kind != Kind::String {
            return invalid(format!(
                "[input.{name}]: `allow_leading_dash` only applies to strings"
            ));
        }
        let numeric = matches!(decl.kind, Kind::Integer | Kind::Number);
        if (decl.minimum.is_some() || decl.maximum.is_some()) && !numeric {
            return invalid(format!(
                "[input.{name}]: `minimum` and `maximum` only apply to numbers"
            ));
        }
        // The pattern must compile on its own first, so it can't close the
        // anchoring group early (`a)|(b`) and match only part of a value.
        let pattern = decl
            .pattern
            .as_deref()
            .map(|pattern| Regex::new(pattern).and_then(|_| Regex::new(&anchored(pattern))))
            .transpose()
            .map_err(|error| Problem::Invalid(format!("[input.{name}]: bad pattern: {error}")))?;
        if let Some(choices) = &decl.choices {
            if choices.is_empty() {
                return invalid(format!("[input.{name}]: `enum` is empty"));
            }
            for choice in choices {
                decl.kind.render(choice).map_err(|error| {
                    Problem::Invalid(format!("[input.{name}]: `enum`: {error}"))
                })?;
                if !decl.allow_leading_dash && choice.as_str().is_some_and(|c| c.starts_with('-')) {
                    return invalid(format!(
                        "[input.{name}]: `enum` choice {choice} starts with `-`: set `allow_leading_dash = true` if the program should get it as an option"
                    ));
                }
            }
        }
        Ok(Self {
            kind: decl.kind,
            pattern,
            minimum: decl.minimum.as_ref().and_then(Number::as_f64),
            maximum: decl.maximum.as_ref().and_then(Number::as_f64),
            choices: decl.choices.clone(),
            allow_leading_dash: decl.allow_leading_dash,
        })
    }

    /// The model's `value` for input `name` as one argument, or why not.
    fn argument(&self, name: &str, value: &Value) -> Result<String, String> {
        let text = self
            .kind
            .render(value)
            .map_err(|error| format!("`{name}`: {error}"))?;
        if let Some(choices) = &self.choices
            && !choices.contains(value)
        {
            let choices: Vec<String> = choices.iter().map(Value::to_string).collect();
            return Err(format!("`{name}` must be one of {}", choices.join(", ")));
        }
        // Strings only. A number can't name an option or carry a path, and
        // a negative one (`-5`) is a real value; `minimum = 0` refuses it.
        if self.kind == Kind::String && !self.allow_leading_dash && text.starts_with('-') {
            return Err(format!(
                "`{name}` can't start with `-`: the program would read it as an option. Rephrase it without the leading `-`."
            ));
        }
        if let Some(pattern) = &self.pattern
            && !pattern.is_match(&text)
        {
            return Err(format!("`{name}` must match the pattern {pattern}"));
        }
        let number = value.as_f64().unwrap_or_default();
        if let Some(minimum) = self.minimum
            && number < minimum
        {
            return Err(format!("`{name}` must be at least {minimum}"));
        }
        if let Some(maximum) = self.maximum
            && number > maximum
        {
            return Err(format!("`{name}` must be at most {maximum}"));
        }
        Ok(text)
    }
}

/// A tool declared in a TOML file. See `examples/tools/` for both shapes:
/// typed `[input.NAME]` tables filling `{NAME}` placeholders, or `help` plus
/// `{args...}`, where the model builds the arguments from the help text.
pub struct ToolFile {
    spec: ToolSpec,
    run: Vec<Arg>,
    inputs: BTreeMap<String, Input>,
    env: Vec<String>,
    limits: Limits,
}

impl ToolFile {
    /// Read and check the file at `path`, running its `help` command if it
    /// has one. The help command, and every call, run within `limits`.
    pub fn load(path: &Path, limits: Limits) -> Result<Self, ToolFileError> {
        let text = std::fs::read_to_string(path).map_err(|source| ToolFileError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text, limits).map_err(|problem| problem.at(path))
    }

    fn parse(text: &str, limits: Limits) -> Result<Self, Problem> {
        let decl: Declaration =
            toml::from_str(text).map_err(|error| Problem::Invalid(error.to_string()))?;
        if !is_tool_name(&decl.name) {
            return invalid(format!(
                "name `{}` must be 1-64 characters of a-z, A-Z, 0-9, `_` or `-`",
                decl.name
            ));
        }
        if let Some(bad) = decl.env.iter().find(|name| !is_env_name(name)) {
            return invalid(format!(
                "`env` entry `{bad}` must be a variable name: letters, digits and `_`, not starting with a digit"
            ));
        }
        let inputs = decl
            .input
            .iter()
            .map(|(name, input)| Ok((name.clone(), Input::compile(name, input)?)))
            .collect::<Result<BTreeMap<_, _>, Problem>>()?;
        let run = arguments(&decl.run, &decl.input)?;
        let parameters = if run.contains(&Arg::Rest) {
            rest_schema(&decl.run)
        } else {
            schema(&decl.input)
        };
        let help = decl.help.as_deref();
        let description = describe(decl.description.as_deref(), help, &decl.env, limits)?;
        Ok(Self {
            spec: ToolSpec {
                name: decl.name,
                description,
                parameters,
            },
            run,
            inputs,
            env: decl.env,
            limits,
        })
    }

    /// The command line for one call, checked against the declaration.
    fn argv(&self, arguments: &Value) -> Result<Vec<String>, String> {
        let none = Map::new();
        let given = match arguments {
            Value::Object(given) => given,
            Value::Null => &none,
            other => return Err(format!("arguments must be a JSON object, got {other}")),
        };
        let expected: Vec<&str> = if self.run.contains(&Arg::Rest) {
            vec!["args"]
        } else {
            self.inputs.keys().map(String::as_str).collect()
        };
        if let Some(unknown) = given.keys().find(|key| !expected.contains(&key.as_str())) {
            return Err(if expected.is_empty() {
                format!("unknown argument `{unknown}`: this tool takes no arguments")
            } else {
                format!(
                    "unknown argument `{unknown}`: this tool takes {}",
                    expected.join(", ")
                )
            });
        }
        let mut argv = Vec::with_capacity(self.run.len());
        for arg in &self.run {
            match arg {
                Arg::Fixed(text) => argv.push(text.clone()),
                Arg::Input(name) => {
                    let value = given
                        .get(name)
                        .ok_or_else(|| format!("missing argument `{name}`"))?;
                    argv.push(self.inputs[name].argument(name, value)?);
                }
                Arg::Rest => {
                    let not_a_list = || "`args` must be a list of strings".to_owned();
                    let items = given
                        .get("args")
                        .and_then(Value::as_array)
                        .ok_or_else(not_a_list)?;
                    for item in items {
                        argv.push(item.as_str().ok_or_else(not_a_list)?.to_owned());
                    }
                }
            }
        }
        Ok(argv)
    }
}

impl Tool for ToolFile {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(&self, arguments: &Value) -> Result<String, String> {
        let argv = self.argv(arguments)?;
        let (program, rest) = argv.split_first().ok_or("nothing to run")?;
        let mut command = isolated(program, &self.env);
        command.args(rest);
        output_of(command, &argv.join(" "), self.limits)
    }
}

/// `run`, checked: a fixed program, whole-argument placeholders that all
/// have inputs, every input used, and `{args...}` only on its own.
fn arguments(run: &[String], inputs: &BTreeMap<String, InputDecl>) -> Result<Vec<Arg>, Problem> {
    let Some(program) = run.first() else {
        return invalid("`run` is empty: it needs at least the program");
    };
    if program == REST || placeholder(program).is_some() {
        return invalid("the program (the first item of `run`) must be fixed, not a placeholder");
    }
    let args = run
        .iter()
        .map(|item| {
            if item == REST {
                return Ok(Arg::Rest);
            }
            if let Some(name) = placeholder(item) {
                return if inputs.contains_key(name) {
                    Ok(Arg::Input(name.to_owned()))
                } else {
                    invalid(format!("`{item}` in `run` has no [input.{name}]"))
                };
            }
            if let Some(name) = inputs
                .keys()
                .find(|name| item.contains(&format!("{{{name}}}")))
            {
                return invalid(format!(
                    "`{item}` in `run`: a placeholder must be a whole argument, like \"{{{name}}}\""
                ));
            }
            Ok(Arg::Fixed(item.clone()))
        })
        .collect::<Result<Vec<_>, Problem>>()?;
    let rests = args.iter().filter(|arg| **arg == Arg::Rest).count();
    if rests > 1 {
        return invalid("`{args...}` can appear only once in `run`");
    }
    if rests == 1 && !inputs.is_empty() {
        return invalid(
            "`{args...}` lets the model supply every argument, so it can't be combined with [input] tables",
        );
    }
    if let Some(unused) = inputs
        .keys()
        .find(|name| !args.contains(&Arg::Input((*name).clone())))
    {
        return invalid(format!("[input.{unused}] is never used in `run`"));
    }
    Ok(args)
}

/// The input name in a `{name}` placeholder.
fn placeholder(item: &str) -> Option<&str> {
    let name = item.strip_prefix('{')?.strip_suffix('}')?;
    is_input_name(name).then_some(name)
}

fn is_input_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The schema for typed inputs: all required, nothing else accepted. Keys
/// go in a fixed order and inputs by name, so the bytes never change.
fn schema(inputs: &BTreeMap<String, InputDecl>) -> Value {
    let properties: Map<String, Value> = inputs
        .iter()
        .map(|(name, input)| (name.clone(), property(input)))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": inputs.keys().collect::<Vec<_>>(),
        "additionalProperties": false,
    })
}

fn property(input: &InputDecl) -> Value {
    let mut property = Map::new();
    property.insert("type".into(), input.kind.name().into());
    if let Some(description) = &input.description {
        property.insert("description".into(), description.trim().into());
    }
    if let Some(choices) = &input.choices {
        property.insert("enum".into(), choices.clone().into());
    }
    if let Some(pattern) = &input.pattern {
        property.insert("pattern".into(), anchored(pattern).into());
    }
    if let Some(minimum) = &input.minimum {
        property.insert("minimum".into(), minimum.clone().into());
    }
    if let Some(maximum) = &input.maximum {
        property.insert("maximum".into(), maximum.clone().into());
    }
    property.into()
}

/// A `pattern` matches the whole value, unlike JSON Schema's, which only
/// searches: there `[a-z]+` accepts `-rf /; x`. Here the pattern guards
/// argv, so it's anchored, and the schema sends the same anchored form so
/// the model and the guard agree.
fn anchored(pattern: &str) -> String {
    format!("^(?:{pattern})$")
}

/// The schema when the model supplies the arguments: one list of strings.
/// No leading-dash check here: `{args...}` is the broad grant, and options
/// are the point of it.
fn rest_schema(run: &[String]) -> Value {
    let before: Vec<&str> = run
        .iter()
        .map(String::as_str)
        .take_while(|item| *item != REST)
        .collect();
    let description = format!(
        "Arguments for `{}`, one per item. They're passed exactly as given, with no shell, so don't quote or escape them.",
        before.join(" ")
    );
    json!({
        "type": "object",
        "properties": {
            "args": {"type": "array", "items": {"type": "string"}, "description": description},
        },
        "required": ["args"],
        "additionalProperties": false,
    })
}

/// A portable environment variable name (POSIX: letters, digits, `_`).
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The author's description, then the program's help text.
fn describe(
    description: Option<&str>,
    help: Option<&[String]>,
    env: &[String],
    limits: Limits,
) -> Result<String, Problem> {
    let mut parts = Vec::new();
    if let Some(description) = description {
        parts.push(description.trim().to_owned());
    }
    if let Some(help) = help {
        let Some((program, args)) = help.split_first() else {
            return invalid("`help` is empty: it needs at least the program");
        };
        let shown = help.join(" ");
        parts.push(format!(
            "`{shown}` says:\n{}",
            help_text(program, args, env, &shown, limits)?
        ));
    }
    if parts.is_empty() {
        return invalid(
            "needs a `description`, a `help` command, or both: the model has to know what the tool does",
        );
    }
    Ok(parts.join("\n\n"))
}

/// Run a help command the same way every time, so its text (which sits in
/// the cached prefix) is byte-stable: C locale, 80 columns, no colour. Many
/// programs print help to stderr or exit non-zero, so any output counts.
fn help_text(
    program: &str,
    args: &[String],
    env: &[String],
    shown: &str,
    limits: Limits,
) -> Result<String, Problem> {
    let mut command = isolated(program, env);
    command
        .args(args)
        .env("LC_ALL", "C")
        .env("COLUMNS", "80")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb");
    let output = capture(command, HELP_LIMIT, Some(limits.timeout))
        .map_err(|error| Problem::Help(format!("could not run `{shown}`: {error}")))?;
    if output.ended == Ended::TimedOut {
        let after = seconds(limits.timeout);
        return Err(Problem::Help(format!("`{shown}` timed out after {after}")));
    }
    let stream = if output.stdout.text().trim().is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    let text = stream.text();
    let text: Vec<&str> = text.lines().map(str::trim_end).collect();
    let text = text.join("\n").trim().to_owned();
    if text.is_empty() {
        return Err(Problem::Help(format!("`{shown}` printed nothing")));
    }
    let text = bounded(text, stream.dropped(), HELP_LIMIT, "help");
    Ok(cap(text, HELP_LIMIT, "help"))
}

#[cfg(test)]
mod tests;
