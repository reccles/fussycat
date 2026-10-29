# fussycat

> A **fussy cut** is a quilt piece cut so one motif sits exactly where you want
> it, the way each block of an agent's context frames one thing for the model.
> The tool is *fussycat*; the pattern it implements is the
> [*Agent Quilt*](docs/agent-quilt.md).

**An AI agent that behaves like a Unix tool.** The task goes in on stdin, the
answer comes out on stdout, and the command line says everything the agent
may know and do. Fussycat makes it easy to chain tools together without a
big fancy framework. Use pipes, files, and commands. To save on tool calls
you can also use `--live`, which automatically puts current state at the
end of the agent's context. This text doesn't pollute the context window:
it's replaced every turn rather than piling up, so you save the tokens tool
calls would cost, and the agent isn't confused by stale state from earlier
turns.

As a quick example:

```sh
git diff | fussycat \
  --model ollama:gpt-oss:20b \
  --stable review-rules.md \
  --stable INTENT.md \
  --live 'git status --short' \
  --tool 'tests=cargo test -q' \
  > review.md
```

Here we pipe the diff into fussycat as the task, for a local model to
review. Two stable markdown files compose our agent's behavior: the review
rules and the project's intent. The `--live` block allows us to declare
context that is refreshed each turn of our agent. This cuts down on tool or
MCP overhead in wasted context: a fresh state is added each agent turn,
replacing the last. That reduces not just the execution time, but the
amount of tokens used, saving you money. Tools can be plain command-line
tools, like the test run here, or for more complex commands a tool can be
defined as a TOML file. And running the tests is *all* this agent can do: a
bare fussycat has no tools, so the command line is the whole list of what
it's allowed. Finally, the answer goes to stdout and the metrics to stderr,
so you can chain any normal Unix commands, or more fussycat calls, into a
pipeline.

## Try it

You need Rust 1.97.1 or newer, and a model that supports tools. This uses
[Ollama](https://ollama.com); Anthropic and Venice work too, and
`fussycat --help` lists every flag.

```sh
ollama pull gpt-oss:20b
cargo install --git https://github.com/reccles/fussycat fussycat
echo "What time is it in UTC?" | fussycat \
  --model ollama:gpt-oss:20b \
  --tool 'now=date -u'
```

## Read more

- [Using fussycat](docs/usage.md): pipes, exit codes, live blocks, limits,
  metrics, providers and keys.
- [Tool files](docs/tools.md): tools with typed inputs that model text can't
  escape.
- [Examples](examples/README.md): runnable scripts, one feature at a time,
  from a first question to a sandboxed run.
- [The Agent Quilt pattern](docs/agent-quilt.md) behind it, and
  [the framework](docs/agent-quilt-framework.md) it implements.
- [Why it exists](context/INTENT.md), [the domain model](context/domain-design.spec.md)
  and [the checks every change passes](context/architecture.eval.md).
- [Contributing](AGENTS.md): the gates and the rules, for people and coding
  agents alike.

[MIT](LICENSE) licensed.
