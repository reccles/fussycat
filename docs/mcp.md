# MCP servers

An [MCP](https://modelcontextprotocol.io) server is a program that offers
tools, and often state you can read, over a standard protocol. There are
servers for GitHub, databases, filesystems, issue trackers and more. With
`--mcp FILE.toml`, fussycat grants a server's tools the way `--tool` grants
a [tool file](tools.md): one file per server, and nothing the file doesn't
name.

## A server file

```toml
name = "github"                                              # prefixes its tools
run = ["npx", "-y", "@modelcontextprotocol/server-github"]   # argv, never a shell
env = ["GITHUB_PERSONAL_ACCESS_TOKEN"]                       # passed through by name
tools = ["search_issues", "get_issue"]                       # what the model may call
```

```sh
echo "Summarise open bugs labelled crash." |
  fussycat --mcp mcp/github.toml
```

| Key | Means |
|---|---|
| `version` | optional: the file format, `1`. A file for a newer format is refused |
| `name` | the server's name, and its tools' prefix: the model calls `github__get_issue`. Letters, digits, `_` and `-`, without `__` |
| `run` | the program and its arguments, run directly, never through a shell |
| `env` | variables passed on from fussycat's environment. The server starts with the same clean environment as every tool, so everything else stays behind |
| `tools` | the allowlist, **required**. `["*"]` grants everything the server offers, but you have to write it. `[]` grants none, for a server you only read live views from |
| `[live.NAME]` | a live view: see below |

The allowlist is the point. A server that adds a tool in its next release
doesn't hand it to your agent; you add it here when you want it. A name on
the list that the server doesn't offer fails the run before any model call
(exit 1), and the message lists what it does offer.

Each granted tool reaches the model with the server's own description and
input schema, so the server, not fussycat, checks the model's arguments.
That's the difference from a tool file: there you write the inputs and
fussycat checks them; here you choose which server to trust, and which of
its tools.

`--mcp mcp/*.toml` grants a folder of servers. Two servers with one `name`
are refused.

## What a server offers

```sh
$ fussycat --list-tools mcp/board.toml
get_current_state (read-only): The board's current state.
move_card (destructive): Moves a card to a column.
```

One tool a line, by the name `tools = [...]` takes, with what the server
marks it, then the first line of its description. It starts the server,
asks, and stops it; no model is called.

## Live views

A server often has state worth seeing every turn: a board, a queue, a
deploy. Declare it as a live view, a call you fix, and fussycat makes it
before every turn and shows the result after the log, like `--live 'cmd'`:

```toml
name = "board"
run = ["board-mcp-server"]
tools = ["move_card", "add_comment"]

[live.state]                        # a tool call, with your arguments
tool = "get_current_state"
arguments = { team = "core" }

[live.columns]                      # or a resource, read by its URI
resource = "board://columns"
```

```sh
fussycat --mcp mcp/board.toml --live mcp:board.state --live 'date -u' ...
```

```text
<live name="mcp:board.state" fetched_at="2026-10-31T14:02:11Z">
team core: {"in_progress": 3, "blocked": 1}
</live>
```

- **Declared in the file, enabled per run.** A view runs only when a run
  names it with `--live mcp:SERVER.VIEW`, so one file serves runs with and
  without it. A view or server that isn't there is a usage error (exit 2).
- **The arguments are yours.** The model can't change them, and the view's
  tool needn't be on the allowlist: the model can see the state without
  being able to call the tool.
- **Views should only read.** A view runs every turn, and fussycat can't
  prove a tool has no side effects. A tool the server itself marks
  destructive is refused as a view.
- **Like any live block:** cut at `--max-tool-output`, bounded by
  `--command-timeout`, and a fetch that fails shows the last good content
  marked stale (or failed, if none ever succeeded) while the run goes on.
  It sits after the log, so a state that changes every turn never costs
  the cache.

## How a run uses a server

1. **Before the first turn,** once the task is read, fussycat starts each
   server in its own process group, initializes it and reads its tool list,
   all within `--command-timeout`. A server that won't start fails the run
   (exit 1), with the first lines of its stderr.
2. **The tool list is fixed for the run.** The granted tools go in the
   cached prefix, sorted like every other tool. A server that says its list
   changed is ignored: the prefix doesn't move mid-run.
3. **Every call** is bounded by `--command-timeout`, and its result cut at
   `--max-tool-output`. A result the server marks as an error goes back to
   the model as an error, like a failing command's. Images and other content
   the model can't read become a note saying what was there.
4. **When the run ends,** the server and everything it started are killed
   with its process group. A stop signal (Ctrl-C, `kill`) reaches it like
   any running command.

Not yet: servers over HTTP, prompts, and servers that ask the model or you
questions (sampling, elicitation).

## Security

A server is a program you choose to run, with the variables you pass it,
for the whole run. Treat a file like a tool file with `{args...}`: the
model can call any allowed tool with any arguments the server accepts.

- **Grant narrowly.** List only the tools the task needs; prefer read-only
  ones, and give write tools to runs that need them.
- **Descriptions are the server's words,** and they go in front of the
  model. A server you don't trust can steer the model through them, as any
  tool output can. Pin the server's version in `run` (`server@1.2.3`) so
  its words don't change under you.
- **Mind the trifecta.** A server that reads private data, plus one that
  fetches the web, plus one that can send or push, is the combination to
  avoid (see the security model in the
  [domain spec](../context/domain-design.spec.md#security-model)).
