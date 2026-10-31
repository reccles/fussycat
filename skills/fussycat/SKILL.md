---
name: fussycat
description: Write and run fussycat agent scripts. fussycat is a CLI agent that reads a task on stdin, writes the answer to stdout, and may only use the tools, context and model its command line grants. Use when asked to build a script, pipeline, cron job or CI step that calls an LLM through fussycat; to write a fussycat tool file (.toml) or MCP server file; or to debug a fussycat run, its exit code or its metrics.
---

# Writing fussycat agent scripts

fussycat is an agent that behaves like a Unix filter. The task goes in on
stdin, the answer comes out on stdout, progress and cost go to stderr, and
the exit code says how the run ended. **The command line is the whole
permission list:** a bare run has no tools, so the model can only answer
from what it was given.

```sh
git diff | fussycat \
  --model ollama:gpt-oss:20b \
  --stable review-rules.md \
  --live 'git status --short' \
  --tool 'tests=cargo test -q' \
  > review.md
```

Run `fussycat --help` for the flags of the installed version; it is the
source of truth if anything here disagrees with it.

## How to build one

1. **The task is stdin.** Pipe the input in (`git diff |`, `< file`,
   `echo "question" |`). An empty task is a usage error (exit 2).
2. **Instructions go in `--stable FILE`**: rules, a style guide, a schema,
   an output format. Read once, in order, and cached by the provider, so a
   loop over many inputs pays for them once. `--stable <(cmd)` works.
3. **Changing state goes in `--live 'cmd'`**: re-run before every turn and
   shown fresh, replacing the last copy. Use it instead of a tool when the
   model only needs to *see* something (status, a queue, the time). A live
   command that fails shows as failed or stale; the run goes on.
4. **Grant the fewest tools that do the job** (see below). Prefer a live
   block to a read tool, and a typed tool file to a broad one.
5. **Pick the model:** `--model PROVIDER:MODEL`, or `FUSSYCAT_MODEL` in
   the environment. Develop against a free local model (`ollama:` or
   `llamacpp:`) and switch providers by changing only `--model`.
6. **Set limits** for anything unattended: `--max-turns` (default 10),
   `--max-cost USD` (paid providers that report cost), `--command-timeout`
   (default 60 s), `--max-output-tokens`.
7. **Act on the exit code** in the script around it.

## Flags

| Flag | What it does |
|---|---|
| `--model P:M` | `ollama:gpt-oss:20b`, `llamacpp:ALIAS`, `anthropic:claude-haiku-5-5`, `venice:claude-haiku-5-5`. Env: `FUSSYCAT_MODEL` |
| `--list-models P` | What a provider can run, with context size and prices. Calls no model |
| `--stable FILE` | A stable block (repeatable, order kept) |
| `--live CMD` / `--live mcp:SERVER.VIEW` | A live block, refreshed every turn (repeatable) |
| `--tool NAME=CMD` | A shell command the model may run. It takes no arguments; the model can trigger it but not change it |
| `--tool FILE.toml` | A tool file: description and typed inputs. `--tool tools/*.toml` grants a folder |
| `--mcp FILE.toml` | An MCP server's allowlisted tools. `--list-tools FILE` shows what it offers |
| `--max-turns N`, `--max-cost USD` | Stop the run (exit 3) |
| `--max-tool-output BYTES` | Cut each tool result and live block, visibly (default 32768) |
| `--command-timeout SECS` | Kill a hung tool or live command (default 60) |
| `--max-output-tokens N` | Cap each reply; a reply cut off at the cap fails the run |
| `--reasoning-effort LEVEL` | `off` `minimal` `low` `medium` `high` `xhigh` `max`, as the model allows |
| `--retries N`, `--provider-timeout SECS` | Model-call resilience (defaults 3; 180 s hosted, 600 s ollama) |
| `--metrics FILE` | Append JSON lines (one per turn, one per run) |
| `-v` / `-q` | A line per turn on stderr / no summary at all |

## Exit codes

| Code | Means | Typical cause |
|---|---|---|
| 0 | Answered | |
| 1 | Failed: the world | unreachable provider, unreadable file, failing key command, empty or cut-off reply, MCP server that won't start |
| 2 | Usage: what you typed | bad flag, empty task, broken tool file, no key command for a paid provider |
| 3 | A limit was reached | `--max-turns`, `--max-cost` |
| 130, 143 | SIGINT, SIGTERM | the running command is killed too |

An empty or cut-off answer is exit 1, so `fussycat ... > out.md && publish
out.md` never publishes half an answer. On the turn that hits a limit, the
model's tool calls are **not** run.

```sh
#!/bin/sh
set -u
status=0
fussycat --stable triage.md --max-turns 6 < errors.log > summary.md || status=$?
case $status in
  0) mail -s "triage" ops < summary.md ;;
  3) echo "triage hit its limit; see the stderr summary" >&2; exit 3 ;;
  *) exit "$status" ;;
esac
```

## Tools

### Shell tools: `--tool 'NAME=COMMAND'`

```sh
echo "What time is it in UTC?" | fussycat --tool 'now=date -u'
```

The command runs exactly as written; nothing the model says reaches it.
`NAME` is 1-64 of `A-Z a-z 0-9 _ -`. Use this when the model only needs to
trigger something.

### Tool files: the model passes input in

A tool file runs a program from an argv list, **never through a shell**,
and checks every argument first, so model text can't become a command or
an option. Write the narrow shape for anything with side effects:

```toml
version = 1
name = "scale_recipe"
description = "Scales a recipe and prints the shopping list."
run = ["pigglywiggly", "-p", "{recipe}", "-n", "{servings}"]

[input.recipe]
type = "string"
description = "The recipe to scale."
enum = ["pancakes", "porridge"]

[input.servings]
type = "integer"
description = "How many people to feed."
minimum = 1
maximum = 50
```

- Each `{input}` becomes exactly one argument. Every input is required.
  Input names are identifiers (letters, digits, `_`).
- Input keys: `type` (`string`, `integer`, `number`, `boolean`),
  `description`, `enum`, `minimum`/`maximum`, `pattern` (a regex anchored
  to the **whole** value), `allow_leading_dash` (off by default: a value
  starting with `-` would be read as an option).
- A bad value runs nothing; the model gets the error back and can retry.
- `run[0]` is found on `PATH` (fussycat's), or is a path relative to the
  working directory.

The **broad shape** hands over a whole program; use it only for programs
that just read and print:

```toml
name = "pigglywiggly"
description = "A recipe scaler that prints shopping lists."
help = ["pigglywiggly", "--help"]   # run once at load; added to the description
run = ["pigglywiggly", "{args...}"]
```

**Environment:** tools start with only `PATH`, `HOME`, `USER`, `LOGNAME`,
`LANG`, `LC_ALL`, `TZ` and `TMPDIR`. A tool that needs a setting names it:
`env = ["FUSSYCAT_MODEL", "FUSSYCAT_OLLAMA_URL"]`.

To pass free text to a shell script safely, hand it over as `"$1"`:

```toml
run = ["sh", "-c", 'printf "%s\n" "$1" | fussycat', "second_opinion", "{question}"]
```

That is also how fussycat runs as a tool of another fussycat: pass the
child its model and *key command* through `env`, never a key.

### MCP servers: `--mcp FILE.toml`

```toml
name = "github"                                              # tools become github__TOOL
run = ["npx", "-y", "@modelcontextprotocol/server-github@1.2.3"]   # pin the version
env = ["GITHUB_PERSONAL_ACCESS_TOKEN"]
tools = ["search_issues", "get_issue"]                       # required allowlist

[live.open]                         # a live view, enabled per run
tool = "search_issues"
arguments = { q = "is:open label:crash" }
```

```sh
fussycat --list-tools mcp/github.toml           # what the server offers
echo "Summarise the crash bugs." | fussycat --mcp mcp/github.toml --live mcp:github.open
```

`tools = ["*"]` grants everything and `[]` grants nothing (views only). A
listed tool the server doesn't offer fails the run. Live views should only
read; a tool the server marks destructive is refused as a view. Stdio
servers only: no HTTP servers yet.

## Keys and providers

A paid provider's key comes from a **command** that prints it, named in
`FUSSYCAT_<PROVIDER>_KEY_COMMAND`. fussycat reads the key through a pipe.

```sh
export FUSSYCAT_ANTHROPIC_KEY_COMMAND='pass show anthropic'
# or: security find-generic-password -s anthropic -w   (macOS Keychain)
# or: op read op://VAULT/ITEM/credential               (1Password)
```

Never put a key in argv, an environment variable, a tracked file or a
script. Local servers need no key: `--ollama-url` / `FUSSYCAT_OLLAMA_URL`
(default `http://localhost:11434`), `--llamacpp-url` /
`FUSSYCAT_LLAMACPP_URL` (default `http://localhost:8080/v1`).

## Security rules

Treat every tool call as if the worst text in the context wrote it: a web
page, a log line or an email in the task can steer the model.

- **Avoid the lethal trifecta.** Don't combine private data, untrusted
  content and a way out (curl, mail, git push, a write-capable MCP tool) in
  one run. Any two are fine. Read the command line to check.
- **Narrow tools for side effects:** typed inputs with `enum`, bounds and
  `pattern`. Broad `{args...}` tools only for read-only programs.
- **Pass credentials by name, per tool** (`env = [...]`), and only those
  the tool needs.
- **A clean environment isn't a sandbox.** Tools run as you, in your
  working directory, and can read `~/.aws` and `~/.ssh`. For untrusted
  input, run fussycat under `sandbox-exec`, `bwrap` or a container; its
  tools inherit the sandbox.
- **Pin MCP servers** to a version: their tool descriptions go in front of
  the model.

## Patterns

```sh
# A warm loop: the rules are cached after the first file.
for f in src/*.rs; do
  fussycat --stable review-rules.md < "$f" > "reviews/${f##*/}.md"
done

# A chain: one agent's answer is the next one's task.
fussycat --stable extract.md < report.txt | fussycat --stable summarise.md

# Cost and cache share across runs that each passed --metrics runs.jsonl.
jq -s 'map(select(.type == "summary").usage) | (map(.cache_read) | add) / (map(.input) | add)' runs.jsonl

# Why runs stopped without answering.
jq -r 'select(.type == "summary" and .outcome != "answered") | .reason.kind' runs.jsonl
```

## Debugging a run

- Run with `-v` to see each turn's tool calls, tokens and cache share.
- **Exit 2:** read stderr; the flag, tool file or task is named.
- **Exit 1 with an empty reply and tools granted:** the model's tool call
  may have been dropped by the server. Try another model, or llama.cpp
  instead of Ollama.
- **Exit 3:** raise `--max-turns`, or give the model what it keeps fetching
  as a `--live` block instead.
- **The model says it can't know something:** it has no tool or block for
  it. Grant one, don't reword the task.
- `--metrics` lines give the stop `reason.kind` (`max_turns`, `provider`,
  `no_answer`, `answer_cut_off`, ...) and a hash per prefix block, to find
  what broke the cache.

## More

The fussycat repository's `docs/usage.md`, `docs/tools.md` and
`docs/mcp.md` cover each part in depth, and `examples/` has one runnable
script per feature.
