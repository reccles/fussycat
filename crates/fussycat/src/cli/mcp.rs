//! `--mcp FILE.toml`, `--live mcp:SERVER.VIEW` and `--list-tools FILE.toml`:
//! MCP servers' files read and checked with the other declarations, and
//! started with the other side effects, once the task is in.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use fussycat_core::toolbox::Tool;

use super::{EXIT_FAILED, EXIT_USAGE, Refused, print};
use crate::reporters::warn;
use crate::system::{Limits, McpError, McpServer, McpView};

/// The servers a run grants, in the order given.
pub(super) type Servers = Vec<Arc<McpServer>>;

/// What marks a `--live` value as an MCP server's view, not a command.
const VIEW_PREFIX: &str = "mcp:";

/// A wrong file is a usage error; one the world failed to provide, or a
/// server that won't start, is a failure ([`McpError::is_usage`]).
impl From<McpError> for Refused {
    fn from(error: McpError) -> Self {
        if error.is_usage() {
            Self::Usage(error.to_string())
        } else {
            Self::Failed(error.to_string())
        }
    }
}

/// Read and check every file. Two servers with one name would give their
/// tools one prefix, so that's refused.
pub(super) fn load(paths: &[PathBuf], limits: Limits) -> Result<Servers, Refused> {
    let mut servers: Servers = Vec::new();
    for path in paths {
        let server = McpServer::load(path, limits)?;
        if servers.iter().any(|known| known.name() == server.name()) {
            return Err(Refused::Usage(format!(
                "two MCP servers are named `{}`",
                server.name()
            )));
        }
        servers.push(server);
    }
    Ok(servers)
}

/// The view a `--live mcp:SERVER.VIEW` value names; `None` for a live
/// command. One that names no granted server, or no view of it, is a
/// usage error.
pub(super) fn view(spelled: &str, servers: &Servers) -> Option<Result<McpView, Refused>> {
    let reference = spelled.strip_prefix(VIEW_PREFIX)?;
    let usage = |message: String| Refused::Usage(format!("--live {spelled}: {message}"));
    Some((|| {
        let (name, view) = reference
            .split_once('.')
            .ok_or_else(|| usage("expected mcp:SERVER.VIEW".into()))?;
        let server = servers
            .iter()
            .find(|server| server.name() == name)
            .ok_or_else(|| {
                let granted: Vec<&str> = servers.iter().map(|server| server.name()).collect();
                usage(format!(
                    "no MCP server `{name}` is granted with --mcp (granted: {})",
                    listed(&granted)
                ))
            })?;
        server.view(view).ok_or_else(|| {
            let views: Vec<&str> = server.view_names().collect();
            usage(format!(
                "MCP server `{name}` declares no [live.{view}] (it declares: {})",
                listed(&views)
            ))
        })
    })())
}

fn listed(names: &[&str]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

/// Start every server: the tools they grant, once each has shown it offers
/// what its file names.
pub(super) async fn start(servers: &Servers) -> Result<Vec<Box<dyn Tool>>, Refused> {
    let mut tools = Vec::new();
    for server in servers {
        tools.extend(server.start().await?);
    }
    Ok(tools)
}

/// `--list-tools FILE`: start the server, print what it offers, one tool a
/// line, and stop it. Calls no model.
pub(super) async fn list_tools(path: &Path, limits: Limits) -> ExitCode {
    let listed = match McpServer::load(path, limits) {
        Ok(server) => server.offered().await,
        Err(error) => Err(error),
    };
    match listed {
        Ok(lines) => {
            let lines: String = lines.into_iter().map(|line| line + "\n").collect();
            print(&lines, ExitCode::SUCCESS)
        }
        Err(error) => {
            let code = if error.is_usage() {
                EXIT_USAGE
            } else {
                EXIT_FAILED
            };
            warn(format_args!("{error}"));
            ExitCode::from(code)
        }
    }
}
