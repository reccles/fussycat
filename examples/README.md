# Examples

Small, runnable shell scripts that show fussycat working. Each one says what
it shows (on stderr), prints fussycat's answer (on stdout), and exits
non-zero if the behaviour it shows can be checked and isn't there: exit
codes, an empty answer, facts that could only have come from a block. They
never check the model's exact wording. **Every user-visible feature ships in
an example here.**

Read them in this order; each builds on the ones before.

1. **[`ask.sh`](ask.sh)**: the whole interface. The task goes in on stdin,
   the answer comes out on stdout, and with no grants the model has no
   tools. Because diagnostics go to stderr, fussycat drops into a pipeline
   like any other filter.
2. **[`shell-tools.sh`](shell-tools.sh)**: deny by default. Asked the time
   with no tools, the model can't know it; granted `--tool 'now=date -u'`,
   it can. A shell tool is your command, exactly as written: nothing the
   model says reaches a shell.
3. **[`context-blocks.sh`](context-blocks.sh)**: what the model sees besides
   the task. `--stable FILE` is read once and cached ahead of the task (any
   file, including `<(cmd)`); `--live 'cmd'` is re-run every turn, and one
   that fails is shown as failed rather than aborting the run.
4. **[`tool-files.sh`](tool-files.sh)**: tools with a description and typed
   inputs, declared in TOML ([`tools/`](tools/)). The narrow shape fills a
   fixed command line from checked inputs; the broad one lets the model read
   the program's `--help`. Broken tool files are refused before any model
   call.
5. **[`limits-and-exit-codes.sh`](limits-and-exit-codes.sh)**: every way a
   run ends, as an exit code scripts can act on (0 answered, 1 failed, 2
   usage, 3 budget), and the caps that make sure it does end: turns, cost,
   tool output, reply length and command timeouts.
6. **[`metrics.sh`](metrics.sh)**: what a run cost and why it was cheap. The
   stderr summary, `--metrics` JSONL for `jq`, and later turns reading the
   prefix back from the provider's cache.
7. **[`isolation.sh`](isolation.sh)**: tools get a clean environment (your
   keys stay with fussycat), a child fussycat works as a tool, and fussycat
   runs under `sandbox-exec` with no web access ([`sandbox/`](sandbox/)).
8. **[`providers.sh`](providers.sh)**: the same agent on other providers.
   `--list-models` shows what you can run (free); then Venice, which reports
   dollars, and Anthropic, with explicit cache breakpoints. **The provider
   runs cost money** and skip without a key command.

## Running

```sh
examples/ask.sh                                     # one
for s in ask shell-tools context-blocks tool-files limits-and-exit-codes \
  metrics isolation providers; do examples/$s.sh || break; done   # all
```

They need a running [Ollama](https://ollama.com) with a model that can call
tools (checked on Ollama 0.40.10, which reports cached tokens). `metrics.sh`
and the paid parts also need `jq`. `isolation.sh` runs its sandbox part on
macOS only.

`lib.sh`, sourced by every script, builds this checkout and runs its
`target/debug/fussycat`, so an example never tests some other installed
release. It sources your **dev env file** (`~/.config/fussycat/dev.env`, or
`FUSSYCAT_DEV_ENV`; start from [`dev.env.example`](dev.env.example)), which
keeps this machine's settings outside the repo, then defaults what's still
unset:

| Variable | Default | |
| --- | --- | --- |
| `FUSSYCAT_OLLAMA_URL` | `http://localhost:11434` | Where Ollama runs |
| `FUSSYCAT_MODEL` | `ollama:gpt-oss:20b` | Must be `ollama:`: the examples are free, so `lib.sh` refuses a paid default |
| `FUSSYCAT_BIN` | this checkout's build | Another binary, to test it on purpose |
| `FUSSYCAT_VENICE_KEY_COMMAND` | unset | Prints a Venice key; turns on the paid Venice parts |
| `FUSSYCAT_VENICE_MODEL` | `claude-haiku-5-5` | The Venice model they use |
| `FUSSYCAT_ANTHROPIC_KEY_COMMAND` | unset | Prints an Anthropic key; turns on the paid Anthropic part |
| `FUSSYCAT_ANTHROPIC_MODEL` | `claude-haiku-5-5` | The Anthropic model it uses |

A key command, never a key: fussycat runs it and reads the key from its
pipe, so the key is never in argv, the environment or a file.

Lint with `shellcheck -x examples/*.sh examples/tools/pigglywiggly`
(`examples/.shellcheckrc` applies); CI runs the same check.
