//! MCP servers granted with `--mcp mcp/x.toml`. The file mirrors a tool
//! file: `name`, `run` (an argv list, never a shell) and `env` (the
//! variables passed on; the server starts in the clean environment like any
//! command). `tools` is the allowlist: each tool on it becomes a [`Tool`]
//! named `SERVER__TOOL`, with the server's own description and schema.
//! `[live.NAME]` tables declare live views, a tool call with the author's
//! fixed arguments or a resource read, which a run enables with
//! `--live mcp:SERVER.NAME`.
//!
//! The server starts before the first turn and stays up for the run; its
//! tool list is read once, so the prefix never changes mid-run. Every call
//! is bounded by `--command-timeout`; tool results are cut by the core, and
//! live views here, to `--max-tool-output`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use fussycat_core::quilt::{Fetch, Source, SourceError};
use fussycat_core::toolbox::{Invocation, Tool, ToolSpec, cap};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ReadResourceRequestParams,
    ResourceContents,
};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::process::{Limits, Server, isolated, seconds, serve};
use super::tool_file::is_identifier;
use super::{check_version, is_tool_name};

/// Between a server's name and its tool's, in the name the model sees.
const SEPARATOR: &str = "__";
/// `tools = ["*"]`: everything the server offers, granted on purpose.
const EVERY_TOOL: &str = "*";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP file `{}`: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("MCP file `{}`: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("MCP server `{server}`: {message}")]
    Server { server: String, message: String },
}

impl McpError {
    /// A file that's wrong is a usage error (exit 2). One the world failed
    /// to provide, unreadable or a server that won't start or lacks a tool,
    /// is a failure (exit 1).
    #[must_use]
    pub fn is_usage(&self) -> bool {
        matches!(self, Self::Invalid { .. })
    }
}

/// The file as written. Unknown keys are refused, so a typo is an error
/// rather than a silently missing setting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    /// The format's version ([`FILE_VERSION`](super::FILE_VERSION)).
    version: Option<u32>,
    name: String,
    run: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    tools: Vec<String>,
    #[serde(default)]
    live: BTreeMap<String, ViewDecl>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewDecl {
    tool: Option<String>,
    arguments: Option<Map<String, Value>>,
    resource: Option<String>,
}

/// What a live view fetches.
#[derive(Debug, Clone, PartialEq)]
enum View {
    /// A tool call with the author's arguments, never the model's.
    Tool {
        name: String,
        arguments: Map<String, Value>,
    },
    Resource(String),
}

/// The tools a file grants.
#[derive(Debug, Clone, PartialEq)]
enum Allowed {
    Every,
    Named(Vec<String>),
}

/// One MCP server, as its file declares it, and once started its
/// connection. Shared by its tools and live views: the last of them
/// dropped stops it.
pub struct McpServer {
    name: String,
    run: Vec<String>,
    env: Vec<String>,
    allowed: Allowed,
    views: BTreeMap<String, View>,
    limits: Limits,
    connection: OnceLock<Connection>,
}

struct Connection {
    client: RunningService<RoleClient, ()>,
    /// Kills the server's process group when dropped.
    _server: Server,
}

impl McpServer {
    /// Read and check the file at `path`. Nothing starts until
    /// [`start`](Self::start); every call to the server runs within
    /// `limits`.
    pub fn load(path: &Path, limits: Limits) -> Result<Arc<Self>, McpError> {
        let text = std::fs::read_to_string(path).map_err(|source| McpError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text, limits)
            .map(Arc::new)
            .map_err(|message| McpError::Invalid {
                path: path.to_owned(),
                message,
            })
    }

    fn parse(text: &str, limits: Limits) -> Result<Self, String> {
        let decl: Declaration = toml::from_str(text).map_err(|error| error.to_string())?;
        check_version(decl.version)?;
        if !is_tool_name(&decl.name) || decl.name.contains(SEPARATOR) {
            return Err(format!(
                "name `{}` must be 1-64 characters of a-z, A-Z, 0-9, `_` or `-`, without `{SEPARATOR}`",
                decl.name
            ));
        }
        if decl.run.first().is_none_or(String::is_empty) {
            return Err("`run` must start with the program to run".into());
        }
        if let Some(bad) = decl.env.iter().find(|name| !is_identifier(name)) {
            return Err(format!(
                "`env` entry `{bad}` must be a variable name: letters, digits and `_`, not starting with a digit"
            ));
        }
        let allowed = if decl.tools.iter().any(|tool| tool == EVERY_TOOL) {
            if decl.tools.len() > 1 {
                return Err(format!(
                    "`tools`: `{EVERY_TOOL}` grants every tool, so it stands alone"
                ));
            }
            Allowed::Every
        } else {
            Allowed::Named(decl.tools)
        };
        let views = decl
            .live
            .into_iter()
            .map(|(name, view)| Ok((name.clone(), view_of(&name, view)?)))
            .collect::<Result<_, String>>()?;
        Ok(Self {
            name: decl.name,
            run: decl.run,
            env: decl.env,
            allowed,
            views,
            limits,
            connection: OnceLock::new(),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether it grants the model any tools (a file may grant only views).
    #[must_use]
    pub fn grants_tools(&self) -> bool {
        self.allowed != Allowed::Named(Vec::new())
    }

    /// The names of its live views, for an error naming what there is.
    pub fn view_names(&self) -> impl Iterator<Item = &str> {
        self.views.keys().map(String::as_str)
    }

    /// Its live view `name` as a source, if the file declares one.
    #[must_use]
    pub fn view(self: &Arc<Self>, name: &str) -> Option<McpView> {
        self.views.contains_key(name).then(|| McpView {
            server: Arc::clone(self),
            name: name.to_owned(),
        })
    }

    /// Start the server and check it offers what the file names: every
    /// tool on the allowlist, and every view's tool, none of which the
    /// server marks destructive (a view runs every turn). The granted
    /// tools, which the toolbox sorts.
    pub async fn start(self: &Arc<Self>) -> Result<Vec<Box<dyn Tool>>, McpError> {
        let offered = self.connect().await?;
        let find = |name: &str| {
            offered
                .iter()
                .find(|tool| tool.name == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = offered.iter().map(|tool| tool.name.as_ref()).collect();
                    self.failed(format!(
                        "it offers no tool `{name}` (it offers: {})",
                        names.join(", ")
                    ))
                })
        };
        let granted: Vec<&rmcp::model::Tool> = match &self.allowed {
            Allowed::Every => offered.iter().collect(),
            Allowed::Named(allowed) => allowed
                .iter()
                .map(|name| find(name))
                .collect::<Result<_, _>>()?,
        };
        for (view, kind) in &self.views {
            let View::Tool { name, .. } = kind else {
                continue;
            };
            let destructive = find(name)?
                .annotations
                .as_ref()
                .and_then(|hints| hints.destructive_hint);
            if destructive == Some(true) {
                return Err(self.failed(format!(
                    "live view `{view}`: the server marks `{name}` destructive, \
                     and a live view runs it every turn"
                )));
            }
        }
        granted
            .into_iter()
            .map(|tool| {
                Ok(Box::new(McpTool {
                    spec: self.spec(tool)?,
                    server: Arc::clone(self),
                    tool: tool.name.to_string(),
                }) as Box<dyn Tool>)
            })
            .collect()
    }

    /// Every tool the server offers, whatever the allowlist says, one line
    /// each for `--list-tools`: its name as `tools = [...]` takes it, what
    /// the server marks it (`read-only`, `destructive`), and the first line
    /// of its description.
    pub async fn offered(&self) -> Result<Vec<String>, McpError> {
        let offered = self.connect().await?;
        Ok(offered
            .iter()
            .map(|tool| {
                let hints = tool.annotations.as_ref();
                let marks = [
                    (hints.and_then(|h| h.read_only_hint), " (read-only)"),
                    (hints.and_then(|h| h.destructive_hint), " (destructive)"),
                ]
                .into_iter()
                .filter_map(|(hint, mark)| (hint == Some(true)).then_some(mark))
                .collect::<String>();
                let description = tool.description.as_deref().unwrap_or_default();
                let first = description.lines().next().unwrap_or_default();
                format!("{}{marks}: {first}", tool.name)
            })
            .collect())
    }

    /// Spawn the server, initialize, and list its tools, all within the
    /// timeout.
    async fn connect(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
        let (program, rest) = self.run.split_first().expect("checked on load");
        let mut command = isolated(program, &self.env);
        command.args(rest);
        let (server, stdout, stdin) = serve(command)
            .map_err(|error| self.failed(format!("could not run `{program}`: {error}")))?;
        let started = tokio::time::timeout(self.limits.timeout, async {
            let client = ().serve((stdout, stdin)).await.map_err(|e| e.to_string())?;
            let tools = client.list_all_tools().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((client, tools))
        })
        .await;
        let (client, tools) = match started {
            Ok(Ok(started)) => started,
            Ok(Err(error)) => return Err(self.failed(with_stderr(&error, &server))),
            Err(_) => {
                let why = format!("didn't start within {}", seconds(self.limits.timeout));
                return Err(self.failed(with_stderr(&why, &server)));
            }
        };
        let connection = Connection {
            client,
            _server: server,
        };
        if self.connection.set(connection).is_err() {
            return Err(self.failed("started twice".into()));
        }
        Ok(tools)
    }

    /// `tool` as the model sees it: prefixed with the server's name.
    fn spec(&self, tool: &rmcp::model::Tool) -> Result<ToolSpec, McpError> {
        let name = format!("{}{SEPARATOR}{}", self.name, tool.name);
        if !is_tool_name(&name) {
            return Err(self.failed(format!(
                "its tool `{}` can't be named `{name}`: a tool name is 1-64 characters \
                 of a-z, A-Z, 0-9, `_` or `-`",
                tool.name
            )));
        }
        Ok(ToolSpec {
            name,
            description: tool.description.as_deref().unwrap_or_default().to_owned(),
            parameters: Value::Object((*tool.input_schema).clone()),
        })
    }

    fn failed(&self, message: String) -> McpError {
        McpError::Server {
            server: self.name.clone(),
            message,
        }
    }

    fn client(&self) -> Result<&RunningService<RoleClient, ()>, String> {
        self.connection
            .get()
            .map(|connection| &connection.client)
            .ok_or_else(|| format!("MCP server `{}` isn't running", self.name))
    }

    /// Call `tool` with `arguments`, within the timeout. The server's own
    /// error result (`isError`) is an `Err`, as a failing command's is.
    async fn call(
        &self,
        shown: &str,
        tool: &str,
        arguments: Map<String, Value>,
    ) -> Result<String, String> {
        let request = CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments);
        let result = self
            .within(shown, self.client()?.call_tool(request))
            .await?;
        let text = text_of(&result);
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }

    async fn read(&self, shown: &str, uri: &str) -> Result<String, String> {
        let request = ReadResourceRequestParams::new(uri.to_owned());
        let result = self
            .within(shown, self.client()?.read_resource(request))
            .await?;
        Ok(result
            .contents
            .iter()
            .map(resource_text)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// `call` within the timeout, its failures said as `shown`'s.
    async fn within<T>(
        &self,
        shown: &str,
        call: impl Future<Output = Result<T, rmcp::ServiceError>>,
    ) -> Result<T, String> {
        match tokio::time::timeout(self.limits.timeout, call).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("`{shown}` failed: {error}")),
            Err(_) => Err(format!(
                "`{shown}` timed out after {}",
                seconds(self.limits.timeout)
            )),
        }
    }
}

fn view_of(name: &str, view: ViewDecl) -> Result<View, String> {
    match view {
        ViewDecl {
            tool: Some(tool),
            arguments,
            resource: None,
        } => Ok(View::Tool {
            name: tool,
            arguments: arguments.unwrap_or_default(),
        }),
        ViewDecl {
            tool: None,
            arguments: None,
            resource: Some(uri),
        } => Ok(View::Resource(uri)),
        ViewDecl {
            tool: None,
            arguments: Some(_),
            resource: Some(_),
        } => Err(format!(
            "[live.{name}]: `arguments` go with `tool`; a resource is read by its URI alone"
        )),
        _ => Err(format!(
            "[live.{name}] needs `tool` (with any `arguments`) or `resource`, not both"
        )),
    }
}

/// `why`, with what the server said on stderr, if anything.
fn with_stderr(why: &str, server: &Server) -> String {
    match server.stderr() {
        said if said.is_empty() => why.to_owned(),
        said => format!("{why}: {said}"),
    }
}

/// A tool result as the model reads it: its text, and a note for what it
/// can't see. A result with only structured content gives its JSON.
fn text_of(result: &CallToolResult) -> String {
    if result.content.is_empty()
        && let Some(structured) = &result.structured_content
    {
        return structured.to_string();
    }
    result
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(image) => format!("[an image ({}), not shown]", image.mime_type),
            ContentBlock::Audio(audio) => format!("[audio ({}), not shown]", audio.mime_type),
            ContentBlock::Resource(embedded) => resource_text(&embedded.resource),
            ContentBlock::ResourceLink(link) => format!("[resource {}]", link.uri),
            _ => "[content of a kind fussycat can't show]".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resource_text(contents: &ResourceContents) -> String {
    match contents {
        ResourceContents::TextResourceContents { text, .. } => text.clone(),
        ResourceContents::BlobResourceContents { uri, mime_type, .. } => format!(
            "[binary resource {uri} ({}), not shown]",
            mime_type.as_deref().unwrap_or("unknown type")
        ),
        _ => "[resource contents of a kind fussycat can't show]".to_owned(),
    }
}

/// One allowed tool of a server.
struct McpTool {
    spec: ToolSpec,
    server: Arc<McpServer>,
    /// Its name on the server, without the prefix.
    tool: String,
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Value) -> Invocation<'a> {
        Box::pin(async move {
            let arguments = match arguments {
                Value::Object(arguments) => arguments.clone(),
                Value::Null => Map::new(),
                other => return Err(format!("arguments must be an object, got {other}")),
            };
            self.server
                .call(&self.spec.name, &self.tool, arguments)
                .await
        })
    }
}

/// A live view (`--live mcp:SERVER.NAME`), fetched every turn and cut to
/// `--max-tool-output` like a live command's output.
pub struct McpView {
    server: Arc<McpServer>,
    name: String,
}

impl Source for McpView {
    fn fetch(&self) -> Fetch<'_> {
        Box::pin(async move {
            let shown = format!("mcp:{}.{}", self.server.name, self.name);
            let fetched = match &self.server.views[&self.name] {
                View::Tool { name, arguments } => {
                    self.server.call(&shown, name, arguments.clone()).await
                }
                View::Resource(uri) => self.server.read(&shown, uri).await,
            };
            let keep = self.server.limits.keep;
            fetched
                .map(|text| cap(text, keep, "output"))
                .map_err(|error| SourceError(cap(error, keep, "error")))
        })
    }
}

#[cfg(test)]
mod tests;
